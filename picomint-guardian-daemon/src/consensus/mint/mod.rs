mod db;
mod rpc;

use std::collections::BTreeMap;

use anyhow::ensure;
use group::Curve;
use picomint_core::mint::config::{
    MintConfig, MintConfigConsensus, MintConfigPrivate, consensus_denominations,
};
use picomint_core::mint::methods::MintMethod;
use picomint_core::mint::{MintInput, MintInputError, MintOutput, MintOutputError, verify_note};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{Amount, OutPoint};
use picomint_sqlite::{DbRead, WriteTx};
use tbs::{AggregatePublicKey, PublicKeyShare, derive_pk_share};

use crate::config::ServerConfig;
use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g2;
use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    BlindedNonceTable, BlindedSignatureShareRestoreTable, BlindedSignatureShareTable,
    IssuanceCounterTable, NoteNonceTable,
};

/// Run DKG for the mint module, producing a fresh `MintConfig` for this peer.
pub async fn distributed_gen(peers: &DkgHandle<'_>) -> anyhow::Result<MintConfig> {
    let mut tbs_sks = BTreeMap::new();
    let mut tbs_agg_pks = BTreeMap::new();
    let mut tbs_pks = BTreeMap::new();

    for denomination in consensus_denominations() {
        let (poly, sk) = peers.run_dkg_g2().await?;

        tbs_sks.insert(denomination, tbs::SecretKeyShare(sk));

        tbs_agg_pks.insert(denomination, AggregatePublicKey(poly[0].to_affine()));

        let pks = peers
            .num_peers()
            .peer_ids()
            .map(|peer| (peer, PublicKeyShare(eval_poly_g2(&poly, &peer))))
            .collect();

        tbs_pks.insert(denomination, pks);
    }

    Ok(MintConfig {
        private: MintConfigPrivate { tbs_sks },
        consensus: MintConfigConsensus {
            tbs_agg_pks,
            tbs_pks,
            input_fee: Amount::from_msat(100),
            output_fee: Amount::from_msat(100),
        },
    })
}

/// Verify our private tbs shares match the public shares in the consensus
/// config.
pub fn validate_config(cfg: &ServerConfig) -> anyhow::Result<()> {
    for denomination in consensus_denominations() {
        let pk = derive_pk_share(&cfg.private.mint.tbs_sks[&denomination]);

        ensure!(
            pk == cfg.consensus.mint.tbs_pks[&denomination][&cfg.private.identity],
            "Mint private key doesn't match pubkey share"
        );
    }

    Ok(())
}

pub fn process_input(
    server: &Server,
    dbtx: &WriteTx,
    input: &MintInput,
) -> Result<(Amount, XOnlyPublicKey), MintInputError> {
    if dbtx
        .insert(&NoteNonceTable, &input.note.nonce, &())
        .is_some()
    {
        return Err(MintInputError::SpentCoin);
    }

    let pk = server
        .cfg
        .consensus
        .mint
        .tbs_agg_pks
        .get(&input.note.denomination)
        .ok_or(MintInputError::InvalidDenomination)?;

    if !verify_note(input.note, *pk) {
        return Err(MintInputError::InvalidSignature);
    }

    let new_count = dbtx
        .remove(&IssuanceCounterTable, &input.note.denomination)
        .unwrap_or(0)
        .checked_sub(1)
        .expect("Failed to decrement issuance counter");

    dbtx.insert(&IssuanceCounterTable, &input.note.denomination, &new_count);

    Ok((input.note.amount(), input.note.nonce))
}

pub fn process_output(
    server: &Server,
    dbtx: &WriteTx,
    output: &MintOutput,
    outpoint: OutPoint,
) -> Result<Amount, MintOutputError> {
    // Signing a blinded nonce twice mints two notes that share a nonce, so
    // spending either strands the other. A client derives nonces from an
    // issuance counter, so a wallet restored without running restore would
    // replay counters it has already used — this turns that into a
    // rejected transaction instead of destroyed funds, and keeps the
    // issuance counter from crediting a note that can never be spent.
    if dbtx
        .insert(&BlindedNonceTable, &output.nonce, &output.denomination)
        .is_some()
    {
        return Err(MintOutputError::ReusedNonce);
    }

    let signature = server
        .cfg
        .private
        .mint
        .tbs_sks
        .get(&output.denomination)
        .map(|key| tbs::sign_message(output.nonce, *key))
        .ok_or(MintOutputError::InvalidDenomination)?;

    dbtx.insert(&BlindedSignatureShareTable, &outpoint, &signature);

    dbtx.insert(
        &BlindedSignatureShareRestoreTable,
        &output.nonce,
        &signature,
    );

    let new_count = dbtx
        .remove(&IssuanceCounterTable, &output.denomination)
        .unwrap_or(0)
        .checked_add(1)
        .expect("Failed to increment issuance counter");

    dbtx.insert(&IssuanceCounterTable, &output.denomination, &new_count);

    Ok(output.amount())
}

pub fn audit(dbtx: &WriteTx) -> i64 {
    dbtx.iter(&IssuanceCounterTable, |r| {
        r.map(|(denomination, count)| -((denomination.amount().msat * count) as i64))
            .sum()
    })
}

pub async fn handle_api(server: &Server, method: MintMethod) -> Result<Vec<u8>, String> {
    match method {
        MintMethod::SignatureShares(req) => handler_async!(signature_shares, server, req).await,
        MintMethod::SignatureSharesRestore(req) => {
            handler!(signature_shares_restore, server, req).await
        }
        MintMethod::SpendState(req) => handler!(spend_state, server, req).await,
        MintMethod::IssuanceState(req) => handler!(issuance_state, server, req).await,
    }
}
