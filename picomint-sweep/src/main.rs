//! Sweeps a decommissioned mint's wallet.
//!
//! After the mint has stopped transacting, every node exports its sweep
//! secret with `picomint-node-cli module onchain sweep`. A threshold of
//! those secrets interpolates into the secret key of the mint's current
//! UTXO, whose public key is the address holding the funds. The tool looks
//! that address up in the UTXO set of the operator's bitcoind, drains it to
//! the destination with a taproot key spend, and broadcasts through the
//! same bitcoind. Secrets never leave the machine.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, bail, ensure};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::key::TapTweak;
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::transaction::Version;
use bitcoin::{
    Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid,
    Witness, taproot,
};
use clap::Parser;
use picomint_core::onchain::SweepSecret;
use picomint_core::{ALLOWED_MINT_SIZES, NumNodes};
use secp256k1::{Keypair, Message, SECP256K1};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tss::interpolate_secret_key;

/// Sweeps a decommissioned mint's wallet with a threshold of its nodes'
/// sweep secrets. Looks the wallet up through bitcoind, drains it to the
/// address and broadcasts; prints the txid.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Number of nodes in the mint; a threshold of their secrets is required
    nodes: usize,
    /// Address to sweep the funds to, on the network bitcoind runs
    address: String,
    /// Bitcoin Core RPC URL with embedded credentials, e.g. http://user:pass@127.0.0.1:8332
    #[arg(long, env = "BITCOIND_URL")]
    bitcoind_url: String,
    /// Fee rate in sat/vB; defaults to bitcoind's estimate for the next three blocks
    #[arg(long)]
    fee_rate: Option<u64>,
    /// A node's sweep secret from `picomint-node-cli module onchain sweep`; repeat once per node
    #[arg(long, required = true)]
    secret: Vec<String>,
}

#[derive(Deserialize)]
struct BlockchainInfo {
    chain: String,
}

#[derive(Deserialize)]
struct ScanTxOutSet {
    unspents: Vec<Unspent>,
}

#[derive(Deserialize)]
struct Unspent {
    txid: Txid,
    vout: u32,
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    amount: Amount,
}

/// `feerate` is in BTC/kvB and absent while bitcoind has no estimate.
#[derive(Deserialize)]
struct EstimateSmartFee {
    feerate: Option<f64>,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<Value>,
}

struct Bitcoind {
    client: reqwest::Client,
    url: String,
}

impl Bitcoind {
    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> anyhow::Result<T> {
        let request = json!({
            "jsonrpc": "1.0",
            "id": "picomint-sweep",
            "method": method,
            "params": params,
        });

        // Bitcoind signals RPC errors with a non-success status but still
        // sends the JSON-RPC error envelope, so decode before checking the
        // status and only surface it when there is no envelope to blame.
        let http_response = self.client.post(&self.url).json(&request).send().await?;

        let status = http_response.status();

        let response: RpcResponse<T> = http_response
            .json()
            .await
            .with_context(|| format!("bitcoind returned {status} with a non-JSON-RPC body"))?;

        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (_, Some(error)) => bail!("bitcoind {method} failed: {error}"),
            _ => bail!("JSON-RPC response carries neither result nor error"),
        }
    }
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

    for secret in &cli.secret {
        let secret = picomint_base32::decode::<SweepSecret>(secret.trim())
            .context("A sweep secret is malformed")?;

        ensure!(
            secret.node.to_usize() < cli.nodes,
            "A sweep secret names node {}, which a mint of {} nodes does not have",
            secret.node,
            cli.nodes
        );

        ensure!(
            shares.insert(secret.node.to_u64(), secret.sks).is_none(),
            "Two sweep secrets are from node {}",
            secret.node
        );
    }

    ensure!(
        shares.len() >= threshold,
        "A mint of {} nodes needs {threshold} sweep secrets, got {}",
        cli.nodes,
        shares.len()
    );

    let keypair = Keypair::from_secret_key(SECP256K1, &interpolate_secret_key(&shares));

    // The mint uses the tweaked aggregate key as the taproot output key
    // directly, without a BIP341 taptweak, so neither do we.
    let output_key = keypair.x_only_public_key().0.dangerous_assume_tweaked();

    let bitcoind = Bitcoind {
        client: reqwest::Client::new(),
        url: cli.bitcoind_url,
    };

    let info: BlockchainInfo = bitcoind.call("getblockchaininfo", json!([])).await?;

    let network = Network::from_core_arg(&info.chain)
        .with_context(|| format!("bitcoind runs an unknown chain {}", info.chain))?;

    let destination = Address::from_str(&cli.address)
        .context("Invalid destination address")?
        .require_network(network)
        .with_context(|| {
            format!("The destination address is not for {network}, which bitcoind runs")
        })?;

    let source = Address::p2tr_tweaked(output_key, network);

    let scan: ScanTxOutSet = bitcoind
        .call(
            "scantxoutset",
            json!(["start", [format!("addr({source})")]]),
        )
        .await?;

    let utxos = scan.unspents;

    ensure!(
        !utxos.is_empty(),
        "No confirmed funds at {source}, the address these secrets reconstruct. Every node must export its secret after the mint's last onchain transaction has confirmed, and every secret must be copied exactly"
    );

    let fee_rate = match cli.fee_rate {
        Some(fee_rate) => fee_rate,
        None => {
            let estimate: EstimateSmartFee = bitcoind
                .call("estimatesmartfee", json!([3, "CONSERVATIVE"]))
                .await?;

            estimate
                .feerate
                .map(|btc_per_kvb| (btc_per_kvb * 100_000.0).ceil() as u64)
                .context("bitcoind has no fee estimate yet; pass --fee-rate")?
                .max(1)
        }
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

    let txid: Txid = bitcoind
        .call("sendrawtransaction", json!([serialize_hex(&tx)]))
        .await?;

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
