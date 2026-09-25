use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1::{Keypair, SECP256K1, SecretKey};
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use picomint_client::gateway::api::await_outgoing_contract;
use picomint_client::lightning::events::{ReceiveEvent, SendRefundEvent, SendSuccessEvent};
use picomint_client::tx::{Input, TxBuilder};
use picomint_client::{Account, Client, Mnemonic, OperationId, TxAcceptEvent};
use picomint_core::config::MintId;
use picomint_core::lightning::contracts::forfeit_message;
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::lightning::methods::{GatewayMethod, InfoResponse, SendRequest, SendResponse};
use picomint_core::lightning::{LightningInput, OutgoingWitness};
use picomint_core::{Amount, wire};
use picomint_encoding::Encodable as _;
use picomint_lnurl::{get_invoice, parse_lnurl, request as lnurl_request, verify_invoice};
use picomint_redb::Database;
use tracing::info;

use crate::cli;
use crate::client::TestClient;
use crate::env::{NUM_ONLINE_NODES, TestEnv, retry};

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    register_gateway(env, &env.gateway_pk)?;
    client_send.lightning_gateway_refresh()?;
    test_payments(env, client_send).await?;
    test_two_clients_pay_one_invoice(env, client_send).await?;
    test_lnurl_daemon_roundtrip(env).await?;
    test_send_lnurl_direct(env, client_send).await?;
    deregister_gateway(env, &env.gateway_pk)?;

    let mock_gw_pk = spawn_mock_gateway(env).await?;

    register_gateway(env, &mock_gw_pk)?;
    client_send.lightning_gateway_refresh()?;
    test_mock_send_exactly_once(client_send).await?;
    test_mock_send_refund_forfeit(client_send).await?;
    test_mock_wrong_network(client_send).await?;
    test_claim_outgoing_contract(client_send).await?;
    deregister_gateway(env, &mock_gw_pk)?;

    test_direct_lightning_payments(env).await?;

    test_analytics_query(env).await?;

    Ok(())
}

fn register_gateway(env: &TestEnv, gateway_pk: &GatewayPk) -> anyhow::Result<()> {
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_lightning_gateway_add(&data_dir, gateway_pk)?;
    }
    Ok(())
}

fn deregister_gateway(env: &TestEnv, gateway_pk: &GatewayPk) -> anyhow::Result<()> {
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_lightning_gateway_remove(&data_dir, gateway_pk)?;
    }
    Ok(())
}

/// Asserts exact row counts in the gateway's on-disk SQLite analytics tables
/// after all real-gateway-driven scenarios in `run_tests` have completed.
///
/// Expected events (mirrored from the gateway's client events):
///  - `test_payments` self-pay no-liquidity → 1 send, 1 send_cancel
///  - `test_payments` no-route              → 1 send, 1 send_cancel
///  - `test_payments` outgoing success      → 1 send, 1 send_success
///  - `test_payments` incoming success      → 1 receive, 1 receive_success
///  - `test_payments` outgoing cancel       → 1 send, 1 send_cancel
///  - `test_lnurl_daemon_roundtrip` → 1 receive, 1 receive_success
///
/// The mock-gateway tests and `test_direct_lightning_payments` don't drive the real
/// gateway's gateway module, so they produce no rows here.
async fn test_analytics_query(env: &TestEnv) -> anyhow::Result<()> {
    info!("lightning: test_analytics_query");

    let db_path = env
        .gateway_data_dir
        .join("analytics")
        .join("analytics.sqlite");
    let conn = rusqlite::Connection::open(&db_path)?;

    let count = |sql: &str| -> anyhow::Result<u64> {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        Ok(n as u64)
    };

    // One table per event, named after its kind
    assert_eq!(count("SELECT COUNT(*) FROM gateway_send")?, 4);
    assert_eq!(count("SELECT COUNT(*) FROM gateway_send_success")?, 1);
    assert_eq!(count("SELECT COUNT(*) FROM gateway_send_cancel")?, 3);
    assert_eq!(count("SELECT COUNT(*) FROM gateway_receive")?, 2);
    assert_eq!(count("SELECT COUNT(*) FROM gateway_receive_success")?, 2);
    assert_eq!(count("SELECT COUNT(*) FROM gateway_receive_failure")?, 0);

    // No views: an operation's outcome is a join on `operation`
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM gateway_send s \
             INNER JOIN gateway_send_success ss USING (operation)"
        )?,
        1
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM gateway_send s \
             INNER JOIN gateway_send_cancel sc USING (operation)"
        )?,
        3
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM gateway_receive r \
             INNER JOIN gateway_receive_success rs USING (operation)"
        )?,
        2
    );

    // Amounts land as integer msat columns
    let sum: i64 = conn.query_row(
        "SELECT SUM(s.amount) FROM gateway_send s \
         INNER JOIN gateway_send_success ss USING (operation)",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(sum as u64, 1_000_000);

    info!("lightning: test_analytics_query passed");

    Ok(())
}

async fn test_direct_lightning_payments(env: &TestEnv) -> anyhow::Result<()> {
    info!("lightning: test_direct_lightning_payments");

    info!("Gateway pays LDK node invoice...");
    {
        let invoice = env.ldk_node.bolt11_payment().receive(
            1_000_000,
            &lightning_invoice::Bolt11InvoiceDescription::Direct(
                lightning_invoice::Description::new(String::new())?,
            ),
            3600,
        )?;

        cli::gateway_ldk_lightning_send(&env.gateway_data_dir, &invoice.to_string())?;
    }

    info!("LDK node pays gateway invoice...");
    {
        let invoice_str = cli::gateway_ldk_lightning_receive(
            &env.gateway_data_dir,
            bitcoin::Amount::from_sat(1_000),
        )?
        .invoice;
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

    info!("lightning: test_direct_lightning_payments passed");

    Ok(())
}

async fn test_payments(env: &TestEnv, client: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_payments");

    info!("Testing self-pay refund when the gateway has no mint liquidity yet...");

    // First scenario in the suite, so the gateway hasn't been funded by the
    // client→LDK send below — its mint balance is zero. The self-pay
    // routes through the gateway as a direct swap, which has to fund the
    // incoming contract from gateway ecash. With no ecash available the
    // gateway must signal a cancel so the client gets its refund.
    {
        let gateway_pk = client.lightning_gateway()?;
        let invoice =
            client.lightning_invoice_receive(gateway_pk, bitcoin::Amount::from_sat(500))?;

        let send_op = client.lightning_invoice_send(gateway_pk, invoice)?;

        client.await_event::<SendRefundEvent>(send_op).await?;
    }

    info!("Testing external-LN refund when LDK has no route to the invoice payee...");

    // Bolt11Invoice signed by a random keypair (via `mock_invoice`); its
    // payee pubkey is not the gateway's node and not in its LDK network
    // graph, so `bolt11_payment().send()` returns
    // `Error::PaymentSendingFailed { RouteNotFound }` synchronously. The
    // gateway must write a cancel so the client gets its refund.
    {
        let invoice = mock_invoice([30; 32], [31; 32], Currency::Regtest);

        let gateway_pk = client.lightning_gateway()?;
        let send_op = client.lightning_invoice_send(gateway_pk, invoice)?;

        client.await_event::<SendRefundEvent>(send_op).await?;
    }

    info!("Testing payment from client to LDK node (funds gateway mint liquidity)...");

    {
        let invoice = env.ldk_node.bolt11_payment().receive(
            1_000_000,
            &lightning_invoice::Bolt11InvoiceDescription::Direct(
                lightning_invoice::Description::new(String::new())?,
            ),
            3600,
        )?;

        let gateway_pk = client.lightning_gateway()?;

        // The account was funded by the onchain suite, so a send-max
        // through this gateway would pay something.
        ensure!(
            client.lightning_lnurl_send_max_amount(gateway_pk)? > Amount::ZERO,
            "max lightning send amount is zero"
        );
        let send_op = client.lightning_invoice_send(gateway_pk, invoice)?;

        client.await_event::<SendSuccessEvent>(send_op).await?;
    }

    info!("Polling gateway mint balance...");

    let mint = env.invite.mint.to_string();
    retry("gateway mint balance", || {
        let mint = mint.clone();
        async move {
            let balance = cli::gateway_mint_balance(&env.gateway_data_dir, &mint)?.balance_msat;
            ensure!(balance.0 > 0, "gateway mint balance is zero");
            Ok(())
        }
    })
    .await?;

    info!("Testing payment from LDK node to client (half of first send)...");

    {
        let gateway_pk = client.lightning_gateway()?;
        let invoice =
            client.lightning_invoice_receive(gateway_pk, bitcoin::Amount::from_sat(500))?;

        env.ldk_node.bolt11_payment().send(&invoice, None)?;

        client
            .await_row::<ReceiveEvent>(&format!("payment_hash = '{}'", invoice.payment_hash()))
            .await?;

        // Verify the freestanding LDK node observes the payment as successful,
        // i.e. the gateway's trailer settled the HTLC back to it via `claim_for_hash`.
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

        let gateway_pk = client.lightning_gateway()?;
        let send_op = client.lightning_invoice_send(gateway_pk, invoice)?;

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

        client.await_event::<SendRefundEvent>(send_op).await?;
    }

    info!("lightning: test_payments passed");

    Ok(())
}

/// Two clients fund a contract for the same invoice. The gateway pays the
/// invoice for whichever request reaches it first and refunds the other
/// on arrival, so exactly one of the two sends succeeds.
async fn test_two_clients_pay_one_invoice(
    env: &TestEnv,
    client_a: &TestClient,
) -> anyhow::Result<()> {
    info!("lightning: test_two_clients_pay_one_invoice");

    let client_b = env.new_client().await?;

    let ecash = client_a.ecash_send(bitcoin::Amount::from_sat(5_000))?;

    client_b
        .await_tx_outcome(client_b.ecash_receive(&ecash)?)
        .await?
        .map_err(|error| anyhow::anyhow!("funding client b was rejected: {error}"))?;

    client_b.lightning_gateway_refresh()?;

    let invoice = env.ldk_node.bolt11_payment().receive(
        1_000_000,
        &lightning_invoice::Bolt11InvoiceDescription::Direct(lightning_invoice::Description::new(
            String::new(),
        )?),
        3600,
    )?;

    let gateway_pk = client_a.lightning_gateway()?;

    let op_a = client_a.lightning_invoice_send(gateway_pk, invoice.clone())?;
    let op_b = client_b.lightning_invoice_send(gateway_pk, invoice)?;

    let (paid_a, paid_b) = tokio::try_join!(
        await_send_outcome(client_a, op_a),
        await_send_outcome(&client_b, op_b),
    )?;

    ensure!(
        paid_a != paid_b,
        "exactly one of the two sends must succeed: a paid {paid_a}, b paid {paid_b}"
    );

    client_b.shutdown().await;

    info!("lightning: test_two_clients_pay_one_invoice passed");

    Ok(())
}

/// Whether the send under `operation` succeeded, once it has either
/// succeeded or been refunded.
async fn await_send_outcome(client: &TestClient, operation: OperationId) -> anyhow::Result<bool> {
    let filter = format!("operation = '{operation}'");

    retry(&format!("send outcome of {operation}"), || async {
        if !client.rows::<SendSuccessEvent>(&filter)?.is_empty() {
            return Ok(true);
        }

        ensure!(
            !client.rows::<SendRefundEvent>(&filter)?.is_empty(),
            "no outcome yet"
        );

        Ok(false)
    })
    .await
}

async fn test_mock_send_exactly_once(client: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_mock_send_exactly_once");

    let invoice = payable_invoice();

    let gateway_pk = client.lightning_gateway()?;
    let send_op = client.lightning_invoice_send(gateway_pk, invoice.clone())?;

    client.await_event::<SendSuccessEvent>(send_op).await?;

    let error = client
        .lightning_invoice_send(gateway_pk, invoice)
        .expect_err("a second send of the same invoice must be refused");

    ensure!(
        error.to_string().contains("already been attempted"),
        "expected InvoiceAlreadyAttempted, got {error}"
    );

    info!("lightning: test_mock_send_exactly_once passed");

    Ok(())
}

async fn test_mock_send_refund_forfeit(client: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_mock_send_refund_forfeit");

    let invoice = unpayable_invoice();
    let gateway_pk = client.lightning_gateway()?;
    let send_op = client.lightning_invoice_send(gateway_pk, invoice)?;

    client.await_event::<SendRefundEvent>(send_op).await?;

    info!("lightning: test_mock_send_refund_forfeit passed");

    Ok(())
}

async fn test_mock_wrong_network(client: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_mock_wrong_network");

    let invoice = signet_invoice();
    let gateway_pk = client.lightning_gateway()?;

    let error = client
        .lightning_invoice_send(gateway_pk, invoice)
        .expect_err("a signet invoice must be refused on regtest");

    ensure!(
        error.to_string().contains("different currency"),
        "expected WrongCurrency, got {error}"
    );

    info!("lightning: test_mock_wrong_network passed");

    Ok(())
}

async fn test_claim_outgoing_contract(client: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_claim_outgoing_contract");

    // Crash-after-claim scenario: the mock spends the outgoing contract with
    // the preimage and then fails the RPC, as a gateway whose response never
    // reaches the client. The client has to learn the preimage from the
    // mint's preimage table alone, and must neither hang on the request nor
    // refund.
    let gateway_pk = client.lightning_gateway()?;
    let send_op = client.lightning_invoice_send(gateway_pk, claim_invoice())?;

    let success = client.await_event::<SendSuccessEvent>(send_op).await?;
    ensure!(
        success["preimage"] == hex::encode(CLAIM_PREIMAGE),
        "the client settled with a preimage other than the one claimed on the mint"
    );

    info!("lightning: test_claim_outgoing_contract passed");

    Ok(())
}

async fn test_lnurl_daemon_roundtrip(env: &TestEnv) -> anyhow::Result<()> {
    info!("lightning: test_lnurl_daemon_roundtrip");

    let client = env.new_client().await?;

    let lnurl = client.lightning_lnurl_receive(&env.lnurl_daemon_url)?;

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

    env.ldk_node
        .bolt11_payment()
        .send(&invoice_response.pr, None)
        .map_err(|e| anyhow::anyhow!("ldk pay: {e:?}"))?;

    // Wait for the scanner to claim the contract.
    client
        .await_row::<ReceiveEvent>(&format!(
            "payment_hash = '{}'",
            invoice_response.pr.payment_hash()
        ))
        .await?;

    // The ?wait long-poll returns once a threshold of nodes hold the funded
    // contract, so the non-wait check below finds it settled for certain.
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

    info!("lightning: test_lnurl_daemon_roundtrip passed");

    Ok(())
}

const GATEWAY_SECRET: [u8; 32] = [1; 32];
const INVOICE_SECRET: [u8; 32] = [2; 32];

// Scenario selectors: embedded in the invoice's `payment_secret` to pick a
// branch in `mock_handler`'s `Send` arm; the preimage defines the invoice's
// `payment_hash` (so the mint's preimage check succeeds server-side
// and each test's operation — derived from the payment hash — is unique).
const PAYABLE_PREIMAGE: [u8; 32] = [10; 32];
const UNPAYABLE_PREIMAGE: [u8; 32] = [11; 32];
const CLAIM_PREIMAGE: [u8; 32] = [12; 32];

async fn test_send_lnurl_direct(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("lightning: test_send_lnurl_direct");

    // The lnurl names this mint, so the sender funds the recipient's
    // incoming contract itself: no gateway, no invoice, no fee.
    let client_receive = env.new_client().await?;

    let lnurl = client_receive.lightning_lnurl_receive(&env.lnurl_daemon_url)?;

    ensure!(
        client_send.lightning_lnurl_mint(&lnurl)? == Some(env.invite.mint),
        "the lnurl must resolve to the test mint",
    );

    ensure!(
        client_send
            .lightning_lnurl_mint("payee@example.com")?
            .is_none(),
        "a foreign lightning address must resolve to no mint",
    );

    let amount = bitcoin::Amount::from_sat(700);

    ensure!(
        client_send
            .lightning_lnurl_send_direct("payee@example.com", amount)
            .is_err(),
        "a direct send to a foreign lightning address must be refused",
    );

    // A direct max pays no gateway fee, so it exceeds the max through
    // the gateway.
    ensure!(
        client_send.lightning_lnurl_send_direct_max_amount()?
            > client_send.lightning_lnurl_send_max_amount(client_send.lightning_gateway()?)?,
        "a direct max must exceed a gateway max",
    );

    let send_op = client_send.lightning_lnurl_send_direct(&lnurl, amount)?;

    client_send.await_event::<TxAcceptEvent>(send_op).await?;

    // The receive logs under its own operation, so it is found by amount:
    // this client has received nothing else.
    let receive = client_receive
        .await_row::<ReceiveEvent>(&format!("amount = {}", amount.to_sat() * 1000))
        .await?;

    ensure!(
        receive["amount"] == amount.to_sat() * 1000 && receive["fee"] == 0,
        "direct send must land in full, got {receive:?}",
    );

    client_receive.shutdown().await;

    info!("lightning: test_send_lnurl_direct passed");

    Ok(())
}

const PAYABLE_PAYMENT_SECRET: [u8; 32] = [211; 32];
const UNPAYABLE_PAYMENT_SECRET: [u8; 32] = [212; 32];
const CLAIM_PAYMENT_SECRET: [u8; 32] = [214; 32];

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

/// Invoice that makes the mock claim the contract on the mint before it
/// fails the RPC.
fn claim_invoice() -> Bolt11Invoice {
    mock_invoice(CLAIM_PREIMAGE, CLAIM_PAYMENT_SECRET, Currency::Regtest)
}

fn signet_invoice() -> Bolt11Invoice {
    mock_invoice(PAYABLE_PREIMAGE, PAYABLE_PAYMENT_SECRET, Currency::Signet)
}

fn mock_invoice(preimage: [u8; 32], payment_secret: [u8; 32], currency: Currency) -> Bolt11Invoice {
    mock_invoice_msat(preimage, payment_secret, currency, 1_000_000)
}

fn mock_invoice_msat(
    preimage: [u8; 32],
    payment_secret: [u8; 32],
    currency: Currency,
    amount_msat: u64,
) -> Bolt11Invoice {
    let sk = SecretKey::from_slice(&INVOICE_SECRET).expect("valid secret");

    InvoiceBuilder::new(currency)
        .description(String::new())
        .payment_hash(sha256::Hash::hash(&preimage))
        .current_timestamp()
        .min_final_cltv_expiry_delta(0)
        .payment_secret(PaymentSecret(payment_secret))
        .amount_milli_satoshis(amount_msat)
        .expiry_time(Duration::from_secs(3600))
        .build_signed(|m| SECP256K1.sign_ecdsa_recoverable(m, &sk))
        .expect("invoice build")
}

/// Spawns a mock gateway via [`picomint_rpc::run_accept_loop`] — same
/// dispatch lifecycle the real gateway daemon uses. Returns the mock's iroh
/// public key for node registration.
///
/// The mock claims contracts through a mint client of its own: the one
/// place the suite drives the client library in-process, and it drives it
/// in the gateway's role.
async fn spawn_mock_gateway(env: &TestEnv) -> anyhow::Result<GatewayPk> {
    let endpoint = Endpoint::builder(N0)
        .alpns(vec![picomint_rpc::ALPN.to_vec()])
        .transport_config(picomint_rpc::transport_config())
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let pk = GatewayPk(endpoint.id());

    let db_dir = env.data_dir.join("mock-gateway");
    tokio::fs::create_dir_all(&db_dir).await?;

    let db = Database::open(db_dir.join("client.redb"))?;

    let client_endpoint = Endpoint::builder(N0)
        .transport_config(picomint_rpc::transport_config())
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let client = Arc::new(Client::new(
        client_endpoint,
        db.clone(),
        Mnemonic::generate(12)?,
    ));

    // A fresh endpoint finds the invite node over mDNS, and the ecash suite
    // may be loading the mint in parallel; a failed add writes nothing, so
    // a second attempt starts clean.
    let mint = retry("mock gateway joins the mint", || async {
        client
            .add_mint(&env.invite, Some(bitcoin::Network::Regtest))
            .await
            .map_err(anyhow::Error::from)
    })
    .await?;

    tokio::spawn(picomint_rpc::run_accept_loop(endpoint, move |method| {
        mock_handler(client.clone(), db.clone(), mint, method)
    }));

    Ok(pk)
}

async fn mock_handler(
    client: Arc<Client>,
    db: Database,
    mint: MintId,
    method: GatewayMethod,
) -> Result<Vec<u8>, String> {
    match method {
        GatewayMethod::Info(_) => {
            let tx_fee = PaymentFee {
                base: picomint_core::Amount::from_sat(2),
                ppm: 3000,
            };
            Ok(InfoResponse {
                info: Some(GatewayInfo {
                    module_public_key: gateway_keypair().x_only_public_key().0,
                    send_fee: tx_fee,
                    receive_fee: tx_fee,
                }),
            }
            .consensus_encode_to_vec())
        }
        GatewayMethod::Send(req) => {
            let payment_secret = req.invoice.bolt11().payment_secret().0;
            if payment_secret == CLAIM_PAYMENT_SECRET {
                claim_outgoing_contract(&client, &db, mint, req)
                    .await
                    .map_err(|e| e.to_string())?;

                return Err("mock gateway crashed after claiming".to_string());
            }
            let result = if payment_secret == UNPAYABLE_PAYMENT_SECRET {
                Err(gateway_keypair().sign_schnorr(forfeit_message(req.outpoint)))
            } else {
                Ok(PAYABLE_PREIMAGE)
            };
            Ok(SendResponse { result }.consensus_encode_to_vec())
        }
        _ => Err("mock gateway does not support this method".to_string()),
    }
}

/// What a real gateway does once it has paid the invoice: spend the
/// outgoing contract with the preimage. The mint holds the contract only
/// once the funding transaction is accepted, which the request does not
/// wait for, so the mint's long-poll for it comes first.
async fn claim_outgoing_contract(
    client: &Client,
    db: &Database,
    mint: MintId,
    req: SendRequest,
) -> anyhow::Result<()> {
    await_outgoing_contract(&client.api(mint)?, req.outpoint).await?;

    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Lightning(LightningInput::Outgoing(
            req.outpoint,
            OutgoingWitness::Claim(CLAIM_PREIMAGE),
        )),
        keypair: gateway_keypair(),
        amount: req.contract.amount + req.contract.fee,
        fee: client
            .config(mint)
            .context("mint is added")?
            .lightning
            .input_fee,
    });

    let dbtx = db.begin_write();

    client
        .ecash_finalize_and_submit_tx(
            mint,
            &dbtx,
            Account::Primary,
            OperationId::new_random(),
            tx_builder,
            false,
            |_| SendSuccessEvent {
                preimage: CLAIM_PREIMAGE,
            },
        )?
        .context("Insufficient funds")?;

    dbtx.commit();

    Ok(())
}
