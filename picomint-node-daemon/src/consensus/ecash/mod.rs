mod db;
mod rpc;

use std::collections::BTreeMap;

use anyhow::ensure;
use group::Curve;
use picomint_core::ecash::config::{
    EcashConfig, EcashConfigConsensus, EcashConfigPrivate, consensus_denominations,
};
use picomint_core::ecash::methods::EcashMethod;
use picomint_core::ecash::{
    EcashInput, EcashInputError, EcashOutput, EcashOutputError, verify_note,
};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{Amount, OutPoint};
use picomint_redb::{DbRead, WriteTx};
use tbs::{AggregatePublicKey, PublicKeyShare, derive_pk_share};

use crate::config::NodeConfig;
use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g2;
use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    BlindedNonceTable, BlindedSignatureShareRestoreTable, BlindedSignatureShareTable,
    IssuanceCounterTable, NoteNonceTable,
};

/// Run DKG for the ecash module, producing a fresh `EcashConfig` for this node.
pub async fn dkg(nodes: &DkgHandle<'_>) -> anyhow::Result<EcashConfig> {
    let mut tbs_sks = BTreeMap::new();
    let mut tbs_agg_pks = BTreeMap::new();
    let mut tbs_pks = BTreeMap::new();

    for denomination in consensus_denominations() {
        let (poly, sk) = nodes.run_dkg_g2().await?;

        tbs_sks.insert(denomination, tbs::SecretKeyShare(sk));

        tbs_agg_pks.insert(denomination, AggregatePublicKey(poly[0].to_affine()));

        let pks = nodes
            .num_nodes()
            .node_ids()
            .map(|node| (node, PublicKeyShare(eval_poly_g2(&poly, &node))))
            .collect();

        tbs_pks.insert(denomination, pks);
    }

    Ok(EcashConfig {
        private: EcashConfigPrivate { tbs_sks },
        consensus: EcashConfigConsensus {
            tbs_agg_pks,
            tbs_pks,
            input_fee: Amount::from_msat(100),
            output_fee: Amount::from_msat(100),
        },
    })
}

/// Verify our private tbs shares match the public shares in the consensus
/// config.
pub fn validate_config(cfg: &NodeConfig) -> anyhow::Result<()> {
    for denomination in consensus_denominations() {
        let pk = derive_pk_share(&cfg.private.ecash.tbs_sks[&denomination]);

        ensure!(
            pk == cfg.consensus.ecash.tbs_pks[&denomination][&cfg.private.identity],
            "Ecash tbs secret key share doesn't match pubkey share"
        );
    }

    Ok(())
}

pub fn process_input(
    server: &Server,
    dbtx: &WriteTx,
    input: &EcashInput,
) -> Result<(Amount, XOnlyPublicKey), EcashInputError> {
    if dbtx
        .insert(&NoteNonceTable, &input.note.nonce, &())
        .is_some()
    {
        return Err(EcashInputError::SpentCoin);
    }

    let pk = server
        .cfg
        .consensus
        .ecash
        .tbs_agg_pks
        .get(&input.note.denomination)
        .ok_or(EcashInputError::InvalidDenomination)?;

    if !verify_note(input.note, *pk) {
        return Err(EcashInputError::InvalidSignature);
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
    output: &EcashOutput,
    outpoint: OutPoint,
) -> Result<Amount, EcashOutputError> {
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
        return Err(EcashOutputError::ReusedNonce);
    }

    let signature = server
        .cfg
        .private
        .ecash
        .tbs_sks
        .get(&output.denomination)
        .map(|key| tbs::sign_message(output.nonce, *key))
        .ok_or(EcashOutputError::InvalidDenomination)?;

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

pub async fn handle_api(server: &Server, method: EcashMethod) -> Result<Vec<u8>, String> {
    match method {
        EcashMethod::Signatures(req) => handler_async!(signatures, server, req).await,
        EcashMethod::SignaturesRestore(req) => handler!(signatures_restore, server, req).await,
        EcashMethod::SpendState(req) => handler!(spend_state, server, req).await,
        EcashMethod::IssuanceState(req) => handler!(issuance_state, server, req).await,
    }
}
