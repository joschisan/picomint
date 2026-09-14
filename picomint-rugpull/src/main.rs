//! Drains a decommissioned mint's wallet.
//!
//! After the mint has stopped transacting, every node exports its rugpull
//! secret with `picomint-node-cli onchain rugpull` into a file. A
//! threshold of those files interpolates into the secret key of the mint's
//! current UTXO, whose public key is the address holding the funds. The
//! tool looks that address up in the UTXO set of the operator's bitcoind,
//! drains it to the destination with a taproot key spend, and broadcasts
//! through the same bitcoind. Secrets never leave the machine.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, ensure};
use bitcoin::absolute::LockTime;
use bitcoin::address::NetworkUnchecked;
use bitcoin::hashes::Hash;
use bitcoin::key::TapTweak;
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::transaction::Version;
use bitcoin::{
    Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, taproot,
};
use clap::Parser;
use picomint_bitcoind::BitcoindClient;
use picomint_core::{ALLOWED_MINT_SIZES, NumNodes};
use picomint_node_cli_core::RugpullResponse;
use secp256k1::{Keypair, Message, SECP256K1};
use serde_json::json;
use tss::interpolate_secret_key;
use url::Url;

/// Drains a decommissioned mint's wallet with a threshold of its nodes'
/// rugpull secrets. Looks the wallet up through bitcoind, drains it to the
/// address and broadcasts; prints the txid.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Number of nodes in the mint; a threshold of their secrets is required
    nodes: usize,
    /// Address to drain the funds to, on the network bitcoind runs
    address: Address<NetworkUnchecked>,
    /// Bitcoin Core RPC URL with embedded credentials, e.g. http://user:pass@127.0.0.1:8332
    #[arg(long, env = "BITCOIND_URL")]
    bitcoind_url: Url,
    /// Defaults to bitcoind's estimate for the next three blocks
    #[arg(long)]
    fee_rate_sat_per_vb: Option<u64>,
    /// A node's rugpull secret file from `picomint-node-cli onchain rugpull`; repeat once per node
    #[arg(long, required = true)]
    secret: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    ensure!(
        ALLOWED_MINT_SIZES.contains(&cli.nodes),
        "A mint has {} nodes, not {}",
        ALLOWED_MINT_SIZES
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        cli.nodes
    );

    let threshold = NumNodes::from(cli.nodes).threshold();

    let mut shares = BTreeMap::new();

    for path in &cli.secret {
        let file =
            std::fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;

        let secret = serde_json::from_slice::<RugpullResponse>(&file)
            .with_context(|| format!("{} is not a rugpull secret file", path.display()))?;

        ensure!(
            secret.node.to_usize() < cli.nodes,
            "A rugpull secret names node {}, which a mint of {} nodes does not have",
            secret.node,
            cli.nodes
        );

        ensure!(
            shares.insert(secret.node.to_u64(), secret.sks).is_none(),
            "Two rugpull secrets are from node {}",
            secret.node
        );
    }

    ensure!(
        shares.len() >= threshold,
        "A mint of {} nodes needs {threshold} rugpull secrets, got {}",
        cli.nodes,
        shares.len()
    );

    let keypair = Keypair::from_secret_key(SECP256K1, &interpolate_secret_key(&shares));

    // The mint uses the tweaked aggregate key as the taproot output key
    // directly, without a BIP341 taptweak, so neither do we.
    let output_key = keypair.x_only_public_key().0.dangerous_assume_tweaked();

    let bitcoind = BitcoindClient::new(cli.bitcoind_url);

    let network = bitcoind.network().await?;

    let destination = cli.address.require_network(network).with_context(|| {
        format!("The destination address is not for {network}, which bitcoind runs")
    })?;

    let source = Address::p2tr_tweaked(output_key, network);

    let utxos = bitcoind.scan_tx_out_set(&source).await?;

    ensure!(
        !utxos.is_empty(),
        "No confirmed funds at {source}, the address these secrets reconstruct. Every node must export its secret after the mint's last onchain transaction has confirmed"
    );

    let fee_rate = match cli.fee_rate_sat_per_vb {
        Some(fee_rate) => fee_rate,
        None => bitcoind
            .get_feerate(3)
            .await?
            .map(|sat_per_kvb| u64::from(sat_per_kvb).div_ceil(1000))
            .context("bitcoind has no fee estimate yet; pass --fee-rate-sat-per-vb")?,
    };

    let total = utxos.iter().map(|utxo| utxo.amount).sum::<Amount>();

    let prevouts: Vec<TxOut> = utxos
        .iter()
        .map(|utxo| TxOut {
            value: utxo.amount,
            script_pubkey: source.script_pubkey(),
        })
        .collect();

    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: utxos
            .iter()
            .map(|utxo| TxIn {
                previous_output: OutPoint::new(utxo.txid, utxo.vout),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                // A key spend witness is one 64-byte signature; size the
                // transaction with a placeholder before signing.
                witness: Witness::from_slice(&[[0u8; 64]]),
            })
            .collect(),
        output: vec![TxOut {
            value: total,
            script_pubkey: destination.script_pubkey(),
        }],
    };

    let fee = Amount::from_sat(tx.vsize() as u64 * fee_rate);

    let value = total
        .checked_sub(fee)
        .filter(|value| *value >= destination.script_pubkey().minimal_non_dust())
        .with_context(|| {
            format!(
                "The {total} at {source} do not cover a {fee} fee at {fee_rate} sat/vB and a spendable output"
            )
        })?;

    tx.output[0].value = value;

    let mut sighash_cache = SighashCache::new(&tx);

    let signatures = (0..utxos.len())
        .map(|index| {
            let sighash = sighash_cache
                .taproot_key_spend_signature_hash(
                    index,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .expect("Every input has a prevout");

            taproot::Signature {
                signature: SECP256K1
                    .sign_schnorr(&Message::from_digest(sighash.to_byte_array()), &keypair),
                sighash_type: TapSighashType::Default,
            }
        })
        .collect::<Vec<_>>();

    for (input, signature) in tx.input.iter_mut().zip(signatures) {
        input.witness = Witness::p2tr_key_spend(&signature);
    }

    let txid = bitcoind.send_raw_transaction(&tx).await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "txid": txid,
            "swept_from": source,
            "value_sat": value.to_sat(),
            "fee_sat": fee.to_sat(),
            "fee_rate_sat_per_vb": fee_rate,
        }))
        .expect("The report is serializable")
    );

    Ok(())
}
