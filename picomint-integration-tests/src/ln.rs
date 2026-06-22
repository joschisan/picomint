use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use async_stream::stream;
use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1::{Keypair, SECP256K1, SecretKey};
use futures::stream::StreamExt;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use picomint_client::ln::events::{ReceiveEvent, SendEvent, SendRefundEvent, SendSuccessEvent};
use picomint_client::ln::{LightningClientModule, SendPaymentError};
use picomint_client::tx::{Input, TxBuilder};
use picomint_client::{Client, OperationId};
use picomint_core::ln::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::ln::methods::{GatewayMethod, InfoResponse, SendResponse};
use picomint_core::ln::{LightningInput, OutgoingWitness};
use picomint_core::{Amount, OutPoint, wire};
use picomint_encoding::Encodable as _;
use picomint_eventlog::{EventLogEntry, EventLogId};
use picomint_lnurl::{get_invoice, parse_lnurl, request as lnurl_request, verify_invoice};
use tracing::info;

use crate::cli;
use crate::env::{NUM_GUARDIANS, TestEnv, retry};

#[derive(Debug)]
#[allow(dead_code)]
enum LnEvent {
    Send(SendEvent),
    SendSuccess(SendSuccessEvent),
    SendRefund(SendRefundEvent),
    Receive(ReceiveEvent),
}

fn ln_event_stream(
    client: &Arc<Client>,
) -> impl futures::Stream<Item = (picomint_core::core::OperationId, LnEvent)> {
    let client = client.clone();
    let notify = client.event_notify();
    let mut next_id = EventLogId::LOG_START;

    stream! {
        loop {
            let notified = notify.notified();
            let events = client.get_event_log(next_id, 100).await;

            for (id, entry) in events {
                next_id = id.saturating_add(1);

                if let Some((op, event)) = try_parse_ln_event(&entry) {
                    yield (op, event);
                }
            }

            notified.await;
        }
    }
}

fn try_parse_ln_event(
    entry: &EventLogEntry,
) -> Option<(picomint_core::core::OperationId, LnEvent)> {
    let op = entry.operation;
    if let Some(e) = entry.to_event() {
        return Some((op, LnEvent::Send(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, LnEvent::SendSuccess(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, LnEvent::SendRefund(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, LnEvent::Receive(e)));
    }
    None
}

pub async fn run_tests(env: &TestEnv, client_send: &Arc<Client>) -> anyhow::Result<()> {
    register_gateway(env, &env.gw_pk)?;
    LightningClientModule::update_gateway_pks(client_send.ln().clone()).await?;
    LightningClientModule::update_gateway_info(client_send.ln().clone()).await;
    test_payments(env, client_send).await?;
    test_lnurl_daemon_roundtrip(env).await?;
    deregister_gateway(env, &env.gw_pk)?;

    let mock_gw_pk = spawn_mock_gateway().await?;

    register_gateway(env, &mock_gw_pk)?;
    LightningClientModule::update_gateway_pks(client_send.ln().clone()).await?;
    LightningClientModule::update_gateway_info(client_send.ln().clone()).await;
    test_mock_send_exactly_once(client_send).await?;
    test_mock_send_refund_forfeit(client_send).await?;
    test_mock_wrong_network(client_send).await?;
    test_claim_outgoing_contract(client_send).await?;
    test_unilateral_refund(env, client_send).await?;
    deregister_gateway(env, &mock_gw_pk)?;

    test_direct_ln_payments(env).await?;

    test_analytics_query(env).await?;

    Ok(())
}

fn register_gateway(env: &TestEnv, gateway_pk: &GatewayPk) -> anyhow::Result<()> {
    for peer in 0..NUM_GUARDIANS {
        let data_dir = cli::guardian_data_dir(&env.data_dir, peer);
        assert!(cli::guardian_ln_gateway_add(&data_dir, gateway_pk)?);
    }
    Ok(())
}

fn deregister_gateway(env: &TestEnv, gateway_pk: &GatewayPk) -> anyhow::Result<()> {
    for peer in 0..NUM_GUARDIANS {
        let data_dir = cli::guardian_data_dir(&env.data_dir, peer);
        assert!(cli::guardian_ln_gateway_remove(&data_dir, gateway_pk)?);
    }
    Ok(())
}

/// Asserts exact row counts in the gateway's in-memory analytics tables
/// after all real-gateway-driven scenarios in `run_tests` have completed.
///
/// Expected events (module = Ln, emitted by the gateway's gw-module):
///  - `test_payments` self-pay no-liquidity → 1 send, 1 send_cancel
///  - `test_payments` no-route              → 1 send, 1 send_cancel
///  - `test_payments` outgoing success      → 1 send, 1 send_success
///  - `test_payments` incoming success      → 1 receive, 1 receive_success, 1 complete
///  - `test_payments` outgoing cancel       → 1 send, 1 send_cancel
///  - `test_lnurl_daemon_roundtrip` → 1 receive, 1 receive_success, 1 complete
///
/// The mock-gateway tests and `test_direct_ln_payments` don't drive the real
/// gateway's gw module, so they produce no rows here.
async fn test_analytics_query(env: &TestEnv) -> anyhow::Result<()> {
    info!("ln: test_analytics_query");

    let db_path = env.gw_data_dir.join("analytics").join("analytics.sqlite");
    let conn = rusqlite::Connection::open(&db_path)?;

    let count = |sql: &str| -> anyhow::Result<u64> {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        Ok(n as u64)
    };

    // Raw event tables
    assert_eq!(count("SELECT COUNT(*) FROM send")?, 4);
    assert_eq!(count("SELECT COUNT(*) FROM send_success")?, 1);
    assert_eq!(count("SELECT COUNT(*) FROM send_cancel")?, 3);
    assert_eq!(count("SELECT COUNT(*) FROM receive")?, 2);
    assert_eq!(count("SELECT COUNT(*) FROM receive_success")?, 2);
    assert_eq!(count("SELECT COUNT(*) FROM receive_failure")?, 0);
    assert_eq!(count("SELECT COUNT(*) FROM receive_refund")?, 0);

    // outgoing_payments / incoming_payments split sends/receives into
    // per-direction views with one row per operation
    assert_eq!(count("SELECT COUNT(*) FROM outgoing_payments")?, 4);
    assert_eq!(count("SELECT COUNT(*) FROM incoming_payments")?, 2);
    assert_eq!(
        count("SELECT COUNT(*) FROM outgoing_payments WHERE status='success'")?,
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM outgoing_payments WHERE status='cancelled'")?,
        3
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM incoming_payments WHERE status='success'")?,
        2
    );

    // Join key sanity — `operation` must match across event tables
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM send s \
             INNER JOIN send_success ss USING (operation)"
        )?,
        1
    );

    // Amount extraction
    let sum: i64 = conn.query_row(
        "SELECT SUM(amount_msat) FROM outgoing_payments WHERE status='success'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(sum as u64, 1_000_000);

    info!("ln: test_analytics_query passed");

    Ok(())
}

async fn test_direct_ln_payments(env: &TestEnv) -> anyhow::Result<()> {
    info!("ln: test_direct_ln_payments");

    info!("Gateway pays LDK node invoice...");
    {
        let invoice = env.ldk_node.bolt11_payment().receive(
            1_000_000,
            &lightning_invoice::Bolt11InvoiceDescription::Direct(
                lightning_invoice::Description::new(String::new())?,
            ),
            3600,
        )?;

        cli::gateway_ldk_ln_send(&env.gw_data_dir, &invoice.to_string())?;
    }

    info!("LDK node pays gateway invoice...");
    {
        let invoice_str = cli::gateway_ldk_ln_receive(&env.gw_data_dir, 1_000_000)?.invoice;
        let invoice: lightning_invoice::Bolt11Invoice = invoice_str.parse()?;

        // The freestanding node may need a moment to consider the channel ready
        // for outbound payments after the gateway-initiated handshake.
        crate::env::retry("ldk node pays gateway", || async {
            env.ldk_node
                .bolt11_payment()
                .send(&invoice, None)
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("send failed: {e:?}"))
        })
        .await?;
    }

    info!("ln: test_direct_ln_payments passed");

    Ok(())
}

async fn test_payments(env: &TestEnv, client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_payments");

    let ln = client.ln();

    let mut events = pin!(ln_event_stream(client));

    info!("Testing self-pay refund when the gateway has no federation liquidity yet...");

    // First scenario in the suite, so the gateway hasn't been funded by the
    // client→LDK send below — its federation balance is zero. The self-pay
    // routes through the gateway as a direct swap, which has to fund the
    // incoming contract from gateway ecash. With no ecash available the
    // gateway must signal a cancel so the client gets a gateway-signed refund
    // (`expired = false`), not a wait-for-CLTV unilateral refund.
    {
        let (gateway_pk, gateway_info) = ln.select_gateway(None)?;
        let invoice = ln
            .receive(gateway_pk, gateway_info.clone(), Amount::from_msat(500_000))
            .await?;

        let send_op = ln.send(gateway_pk, gateway_info, invoice).await?;

        let Some((op, LnEvent::Send(_))) = events.next().await else {
            panic!("Expected Send event");
        };
        assert_eq!(op, send_op);

        let Some((op, LnEvent::SendRefund(refund))) = events.next().await else {
            panic!("Expected SendRefund event");
        };
        assert_eq!(op, send_op);
        assert!(
            !refund.expired,
            "expected gateway-signed cancel, got CLTV-expiry refund",
        );
    }

    info!("Testing external-LN refund when LDK has no route to the invoice payee...");

    // Bolt11Invoice signed by a random keypair (via `mock_invoice`); its
    // payee pubkey is not in the gateway's LDK network graph, so
    // `is_direct_swap = false` and `bolt11_payment().send()` returns
    // `Error::PaymentSendingFailed { RouteNotFound }` synchronously. The
    // gateway must write a cancel so the client gets a gateway-signed
    // refund (`expired = false`) without waiting for CLTV expiry.
    {
        let invoice = mock_invoice([30; 32], [31; 32], Currency::Regtest);

        let (gateway_pk, gateway_info) = ln.select_gateway(Some(&invoice))?;
        let send_op = ln.send(gateway_pk, gateway_info, invoice).await?;

        let Some((op, LnEvent::Send(_))) = events.next().await else {
            panic!("Expected Send event");
        };
        assert_eq!(op, send_op);

        let Some((op, LnEvent::SendRefund(refund))) = events.next().await else {
            panic!("Expected SendRefund event");
        };
        assert_eq!(op, send_op);
        assert!(
            !refund.expired,
            "expected gateway-signed cancel, got CLTV-expiry refund",
        );
    }

    info!("Testing payment from client to LDK node (funds gateway federation liquidity)...");

    {
        let invoice = env.ldk_node.bolt11_payment().receive(
            1_000_000,
            &lightning_invoice::Bolt11InvoiceDescription::Direct(
                lightning_invoice::Description::new(String::new())?,
            ),
            3600,
        )?;

        let (gateway_pk, gateway_info) = ln.select_gateway(Some(&invoice))?;
        let send_op = ln.send(gateway_pk, gateway_info, invoice).await?;

        let Some((op, LnEvent::Send(_))) = events.next().await else {
            panic!("Expected Send event");
        };
        assert_eq!(op, send_op);

        let Some((op, LnEvent::SendSuccess(_))) = events.next().await else {
            panic!("Expected SendSuccess event");
        };
        assert_eq!(op, send_op);
    }

    info!("Polling gateway federation balance...");

    let federation = env.invite.federation.to_string();
    retry("gateway federation balance", || {
        let federation = federation.clone();
        async move {
            let balance =
                cli::gateway_federation_balance(&env.gw_data_dir, &federation)?.balance_msat;
            ensure!(balance.msat > 0, "gateway federation balance is zero");
            Ok(())
        }
    })
    .await?;

    info!("Testing payment from LDK node to client (half of first send)...");

    {
        let (gateway_pk, gateway_info) = ln.select_gateway(None)?;
        let invoice = ln
            .receive(gateway_pk, gateway_info, Amount::from_msat(500_000))
            .await?;

        env.ldk_node.bolt11_payment().send(&invoice, None)?;

        let Some((_op, LnEvent::Receive(_))) = events.next().await else {
            panic!("Expected Receive event");
        };

        // Verify the freestanding LDK node observes the payment as successful,
        // i.e. the gateway settled the HTLC back to it via the CompleteSM.
        let payment_hash = lightning_types::payment::PaymentHash(*invoice.payment_hash().as_ref());
        loop {
            let event = env.ldk_node.next_event_async().await;
            env.ldk_node.event_handled()?;
            if let ldk_node::Event::PaymentSuccessful {
                payment_hash: hash, ..
            } = event
                && hash == payment_hash
            {
                break;
            }
        }
    }

    info!("Testing refund when the payee fails the payment...");

    {
        let payment_hash = lightning_types::payment::PaymentHash([0; 32]);

        let invoice = env.ldk_node.bolt11_payment().receive_for_hash(
            1_000_000,
            &lightning_invoice::Bolt11InvoiceDescription::Direct(
                lightning_invoice::Description::new(String::new())?,
            ),
            3600,
            payment_hash,
        )?;

        let (gateway_pk, gateway_info) = ln.select_gateway(Some(&invoice))?;
        let send_op = ln.send(gateway_pk, gateway_info, invoice).await?;

        let Some((op, LnEvent::Send(_))) = events.next().await else {
            panic!("Expected Send event");
        };
        assert_eq!(op, send_op);

        // Wait until the HTLC is actually held by LDK, then fail it. Failing
        // before the HTLC arrives is a no-op in LDK's ChannelManager, so the
        // HTLC would sit held and the contract would never cancel.
        loop {
            let event = env.ldk_node.next_event_async().await;
            env.ldk_node.event_handled()?;
            if let ldk_node::Event::PaymentClaimable {
                payment_hash: hash, ..
            } = event
                && hash == payment_hash
            {
                break;
            }
        }
        env.ldk_node.bolt11_payment().fail_for_hash(payment_hash)?;

        let Some((op, LnEvent::SendRefund(_))) = events.next().await else {
            panic!("Expected SendRefund event");
        };
        assert_eq!(op, send_op);
    }

    info!("ln: test_payments passed");

    Ok(())
}

/// Consume the stream until an entry for `op` matches `predicate`, and
/// return that entry. Skips events from other operations (the shared
/// `client_send` accumulates history across tests).
async fn wait_ln_event<S>(
    events: &mut std::pin::Pin<&mut S>,
    op: OperationId,
    predicate: impl Fn(&LnEvent) -> bool,
) -> LnEvent
where
    S: futures::Stream<Item = (OperationId, LnEvent)>,
{
    loop {
        let Some((event_op, event)) = events.next().await else {
            panic!("event stream ended while waiting for op {op:?}");
        };

        if event_op == op && predicate(&event) {
            return event;
        }
    }
}

async fn wait_tx_accepted(
    client: &Arc<Client>,
    op: OperationId,
    txid: picomint_core::TransactionId,
) {
    let mut stream = client.subscribe_operation_events(op);

    while let Some(entry) = stream.next().await {
        if let Some(ev) = entry.to_event::<picomint_client::TxAcceptEvent>()
            && ev.txid == txid
        {
            return;
        }

        if let Some(ev) = entry.to_event::<picomint_client::TxRejectEvent>()
            && ev.txid == txid
        {
            panic!("tx {txid} rejected: {}", ev.error);
        }
    }

    panic!("operation event stream ended");
}

async fn test_mock_send_exactly_once(client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_mock_send_exactly_once");

    let ln = client.ln();

    let invoice = payable_invoice();

    let mut events = pin!(ln_event_stream(client));

    let (gateway_pk, gateway_info) = ln.select_gateway(Some(&invoice))?;
    let send_op = ln
        .send(gateway_pk, gateway_info.clone(), invoice.clone())
        .await?;

    wait_ln_event(&mut events, send_op, |e| matches!(e, LnEvent::Send(_))).await;
    wait_ln_event(&mut events, send_op, |e| {
        matches!(e, LnEvent::SendSuccess(_))
    })
    .await;

    match ln.send(gateway_pk, gateway_info, invoice).await {
        Err(SendPaymentError::InvoiceAlreadyAttempted(op)) => assert_eq!(op, send_op),
        other => panic!("Expected InvoiceAlreadyAttempted, got {other:?}"),
    }

    info!("ln: test_mock_send_exactly_once passed");

    Ok(())
}

async fn test_mock_send_refund_forfeit(client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_mock_send_refund_forfeit");

    let mut events = pin!(ln_event_stream(client));

    let invoice = unpayable_invoice();
    let (gateway_pk, gateway_info) = client.ln().select_gateway(Some(&invoice))?;
    let send_op = client.ln().send(gateway_pk, gateway_info, invoice).await?;

    wait_ln_event(&mut events, send_op, |e| matches!(e, LnEvent::Send(_))).await;
    wait_ln_event(&mut events, send_op, |e| {
        matches!(e, LnEvent::SendRefund(_))
    })
    .await;

    info!("ln: test_mock_send_refund_forfeit passed");

    Ok(())
}

async fn test_mock_wrong_network(client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_mock_wrong_network");

    let invoice = signet_invoice();
    let (gateway_pk, gateway_info) = client.ln().select_gateway(Some(&invoice))?;

    match client.ln().send(gateway_pk, gateway_info, invoice).await {
        Err(SendPaymentError::WrongCurrency {
            invoice_currency: Currency::Signet,
            federation_currency: Currency::Regtest,
        }) => {}
        other => panic!("Expected WrongCurrency, got {other:?}"),
    }

    info!("ln: test_mock_wrong_network passed");

    Ok(())
}

async fn test_claim_outgoing_contract(client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_claim_outgoing_contract");

    let ln = client.ln();

    let mut events = pin!(ln_event_stream(client));

    // Crash scenario: mock HTTP-500s on `Send`, so the client loops
    // retrying indefinitely — giving us room to claim the on-chain contract
    // manually before the client ever sees a gateway response.
    let preimage = [12u8; 32];

    let invoice = crash_invoice(preimage);
    let (gateway_pk, gateway_info) = ln.select_gateway(Some(&invoice))?;
    let send_op = ln.send(gateway_pk, gateway_info, invoice).await?;

    let send_event =
        match wait_ln_event(&mut events, send_op, |e| matches!(e, LnEvent::Send(_))).await {
            LnEvent::Send(e) => e,
            _ => unreachable!(),
        };

    let outpoint = OutPoint {
        txid: send_event.txid,
        out_idx: 0,
    };

    // Wait for the outgoing-contract tx to be accepted before we try to spend
    // it as an input.
    wait_tx_accepted(client, send_op, send_event.txid).await;

    // `SendEvent.amount` is already `send_fee.add_to(invoice_amount)` —
    // i.e. the on-chain contract amount. No further fee addition here.
    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Ln(LightningInput::Outgoing(
            outpoint,
            OutgoingWitness::Claim(preimage),
        )),
        keypair: gateway_keypair(),
        amount: send_event.amount,
        fee: ln.input_fee(),
    });

    let dbtx = client.db().begin_write();

    client
        .mint()
        .finalize_and_submit_tx(&dbtx, OperationId::new_random(), tx_builder, |_| {
            SendSuccessEvent { preimage }
        })
        .context("Insufficient funds")?;

    dbtx.commit();

    wait_ln_event(&mut events, send_op, |e| {
        matches!(e, LnEvent::SendSuccess(_))
    })
    .await;

    info!("ln: test_claim_outgoing_contract passed");

    Ok(())
}

async fn test_unilateral_refund(env: &TestEnv, client: &Arc<Client>) -> anyhow::Result<()> {
    info!("ln: test_unilateral_refund");

    let mut events = pin!(ln_event_stream(client));

    // Same crash scenario — the mock never settles, and without any on-chain
    // preimage reveal the contract must eventually expire so the client can
    // pull its funds back via `OutgoingWitness::Refund`.
    let invoice = crash_invoice([13; 32]);
    let (gateway_pk, gateway_info) = client.ln().select_gateway(Some(&invoice))?;
    let send_op = client.ln().send(gateway_pk, gateway_info, invoice).await?;

    wait_ln_event(&mut events, send_op, |e| matches!(e, LnEvent::Send(_))).await;

    // Contract expiry = consensus_block_count + expiry_delta +
    // CONTRACT_CONFIRMATION_BUFFER = +62 blocks with the mock's settings.
    // Mine 100 so the consensus block count comfortably crosses it.
    env.mine_blocks(100);

    wait_ln_event(&mut events, send_op, |e| {
        matches!(e, LnEvent::SendRefund(_))
    })
    .await;

    info!("ln: test_unilateral_refund passed");

    Ok(())
}

async fn test_lnurl_daemon_roundtrip(env: &TestEnv) -> anyhow::Result<()> {
    info!("ln: test_lnurl_daemon_roundtrip");

    // Fresh client so the receive-event stream starts empty.
    let client = env.new_client(None, false).await?;

    let lnurl_daemon: String = env.lnurl_daemon_url.parse()?;

    let lnurl = client
        .ln()
        .generate_lnurl(lnurl_daemon)
        .await
        .map_err(|e| anyhow::anyhow!("generate_lnurl: {e}"))?;

    let pay_url = parse_lnurl(&lnurl).ok_or_else(|| anyhow::anyhow!("parse_lnurl"))?;

    let pay_response = lnurl_request(&pay_url).await.map_err(anyhow::Error::msg)?;

    let invoice_response = get_invoice(&pay_response, 500_000)
        .await
        .map_err(anyhow::Error::msg)?;

    let verify_url = invoice_response
        .verify
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing verify url"))?;

    // Pre-payment: verify endpoint returns unsettled + no preimage.
    let pre = verify_invoice(&verify_url)
        .await
        .map_err(anyhow::Error::msg)?;

    ensure!(!pre.settled, "verify should not be settled pre-payment");
    ensure!(
        pre.preimage.is_none(),
        "preimage should be absent pre-payment"
    );

    // Long-poll `?wait` in parallel with the payment — must return the same
    // settled response the post-payment GET sees.
    let wait_task = {
        let url = format!("{verify_url}?wait");
        tokio::spawn(async move { verify_invoice(&url).await })
    };

    let mut events = pin!(ln_event_stream(&client));

    env.ldk_node
        .bolt11_payment()
        .send(&invoice_response.pr, None)
        .map_err(|e| anyhow::anyhow!("ldk pay: {e:?}"))?;

    // Wait for the scanner to claim the contract. Fresh client = no
    // historical receive events, so the first ReceiveEvent is ours.
    loop {
        let Some((_, ev)) = events.next().await else {
            panic!("event stream ended");
        };

        if matches!(ev, LnEvent::Receive(_)) {
            break;
        }
    }

    // The ?wait long-poll guarantees the gateway has logged ReceiveSuccessEvent
    // before we do the non-wait check below. Without this ordering the non-wait
    // GET races against the gateway's threshold decryption (which requires a
    // network round trip to all guardians) and can return settled=false even
    // though the client scanner already fired ReceiveEvent locally.
    let waited = wait_task.await?.map_err(anyhow::Error::msg)?;

    // Post-payment: verify endpoint reflects the preimage, which hashes
    // back to the invoice's payment hash.
    let post = verify_invoice(&verify_url)
        .await
        .map_err(anyhow::Error::msg)?;

    ensure!(post.settled, "verify should be settled post-payment");

    let preimage = post
        .preimage
        .ok_or_else(|| anyhow::anyhow!("no preimage"))?;

    ensure!(
        sha256::Hash::hash(&preimage) == *invoice_response.pr.payment_hash(),
        "preimage doesn't match invoice hash"
    );

    assert_eq!(waited, post);

    client.shutdown().await;

    info!("ln: test_lnurl_daemon_roundtrip passed");

    Ok(())
}

const GATEWAY_SECRET: [u8; 32] = [1; 32];
const INVOICE_SECRET: [u8; 32] = [2; 32];

// Scenario selectors: embedded in the invoice's `payment_secret` to pick a
// branch in `mock_handler`'s `Send` arm; the preimage defines the invoice's
// `payment_hash` (so the federation's preimage check succeeds server-side
// and each test's operation — derived from the payment hash — is unique).
const PAYABLE_PREIMAGE: [u8; 32] = [10; 32];
const UNPAYABLE_PREIMAGE: [u8; 32] = [11; 32];

const PAYABLE_PAYMENT_SECRET: [u8; 32] = [211; 32];
const UNPAYABLE_PAYMENT_SECRET: [u8; 32] = [212; 32];
const CRASH_PAYMENT_SECRET: [u8; 32] = [213; 32];

fn gateway_keypair() -> Keypair {
    SecretKey::from_slice(&GATEWAY_SECRET)
        .expect("32-byte secret within curve order")
        .keypair(SECP256K1)
}

fn payable_invoice() -> Bolt11Invoice {
    mock_invoice(PAYABLE_PREIMAGE, PAYABLE_PAYMENT_SECRET, Currency::Regtest)
}

fn unpayable_invoice() -> Bolt11Invoice {
    mock_invoice(
        UNPAYABLE_PREIMAGE,
        UNPAYABLE_PAYMENT_SECRET,
        Currency::Regtest,
    )
}

/// Invoice that triggers the mock's crash branch (HTTP 500, gateway never
/// resolves). Each caller supplies its own preimage so its operation
/// (derived from the payment hash) is distinct.
fn crash_invoice(preimage: [u8; 32]) -> Bolt11Invoice {
    mock_invoice(preimage, CRASH_PAYMENT_SECRET, Currency::Regtest)
}

fn signet_invoice() -> Bolt11Invoice {
    mock_invoice(PAYABLE_PREIMAGE, PAYABLE_PAYMENT_SECRET, Currency::Signet)
}

fn mock_invoice(preimage: [u8; 32], payment_secret: [u8; 32], currency: Currency) -> Bolt11Invoice {
    let sk = SecretKey::from_slice(&INVOICE_SECRET).expect("valid secret");

    InvoiceBuilder::new(currency)
        .description(String::new())
        .payment_hash(sha256::Hash::hash(&preimage))
        .current_timestamp()
        .min_final_cltv_expiry_delta(0)
        .payment_secret(PaymentSecret(payment_secret))
        .amount_milli_satoshis(1_000_000)
        .expiry_time(Duration::from_secs(3600))
        .build_signed(|m| SECP256K1.sign_ecdsa_recoverable(m, &sk))
        .expect("invoice build")
}

/// Spawns a mock gateway via [`picomint_rpc::run_accept_loop`] — same
/// dispatch lifecycle the real gateway daemon uses. Returns the mock's iroh
/// public key for guardian registration.
async fn spawn_mock_gateway() -> anyhow::Result<GatewayPk> {
    let endpoint = Endpoint::builder(N0)
        .alpns(vec![picomint_rpc::ALPN.to_vec()])
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let pk = GatewayPk(endpoint.id());

    tokio::spawn(picomint_rpc::run_accept_loop(endpoint, 1000, mock_handler));

    Ok(pk)
}

async fn mock_handler(method: GatewayMethod) -> Result<Vec<u8>, String> {
    match method {
        GatewayMethod::Info(_) => {
            // Short expiry deltas keep the unilateral-refund test
            // fast — the federation's consensus block count must advance
            // past the contract's expiry for `await_preimage` to
            // return `None`.
            let tx_fee = PaymentFee {
                base: picomint_core::Amount::from_sat(2),
                ppm: 3000,
            };
            Ok(InfoResponse {
                info: Some(GatewayInfo {
                    lightning_public_key: gateway_keypair().public_key(),
                    module_public_key: gateway_keypair().x_only_public_key().0,
                    send_fee: tx_fee,
                    receive_fee: tx_fee,
                    ln_fee: tx_fee,
                    expiry_delta: 50,
                }),
            }
            .consensus_encode_to_vec())
        }
        GatewayMethod::Send(req) => {
            let payment_secret = req.invoice.bolt11().payment_secret().0;
            if payment_secret == CRASH_PAYMENT_SECRET {
                return Err("mock gateway crashed".to_string());
            }
            let result = if payment_secret == UNPAYABLE_PAYMENT_SECRET {
                Err(gateway_keypair().sign_schnorr(req.contract.forfeit_message()))
            } else {
                Ok(PAYABLE_PREIMAGE)
            };
            Ok(SendResponse { result }.consensus_encode_to_vec())
        }
        _ => Err("mock gateway does not support this method".to_string()),
    }
}
