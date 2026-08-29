mod db;
mod rpc;

use std::collections::BTreeMap;

use anyhow::ensure;
use group::Curve;
use picomint_core::mint::config::{
    MintConfig, MintConfigConsensus, MintConfigPrivate, consensus_denominations,
};
use picomint_core::mint::methods::MintMethod;
use picomint_core::mint::{
    MintConsensusItem, MintInput, MintInputError, MintOutput, MintOutputError, verify_note,
};
use picomint_core::module::{InputMeta, TxItemAmounts};
use picomint_core::{Amount, OutPoint, PeerId};
use picomint_redb::{Database, ReadTx, WriteTx};
use tbs::{AggregatePublicKey, PublicKeyShare, derive_pk_share};

use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g2;
use crate::{handler, handler_async};

use self::db::{
    BlindedNonceTable, BlindedSignatureShareRestoreTable, BlindedSignatureShareTable,
    IssuanceCounterTable, NoteNonceKey, NoteNonceTable,
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
pub fn validate_config(identity: &PeerId, cfg: &MintConfig) -> anyhow::Result<()> {
    for denomination in consensus_denominations() {
        let pk = derive_pk_share(&cfg.private.tbs_sks[&denomination]);

        ensure!(
            pk == cfg.consensus.tbs_pks[&denomination][identity],
            "Mint private key doesn't match pubkey share"
        );
    }

    Ok(())
}

pub struct Mint {
    cfg: MintConfig,
    db: Database,
}

impl Mint {
    pub fn new(cfg: MintConfig, db: Database) -> Self {
        Self { cfg, db }
    }
}

impl Mint {
    pub async fn consensus_proposal(&self, _dbtx: &ReadTx) -> Vec<MintConsensusItem> {
        Vec::new()
    }

    pub async fn process_consensus_item(
        &self,
        _dbtx: &WriteTx,
        _peer: PeerId,
        consensus_item: MintConsensusItem,
    ) -> anyhow::Result<()> {
        match consensus_item {}
    }

    pub async fn process_input(
        &self,
        dbtx: &WriteTx,
        input: &MintInput,
    ) -> Result<InputMeta, MintInputError> {
        if dbtx
            .insert(&NoteNonceTable, &NoteNonceKey(input.note.nonce), &())
            .is_some()
        {
            return Err(MintInputError::SpentCoin);
        }

        let pk = self
            .cfg
            .consensus
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

        let amount = input.note.amount();

        Ok(InputMeta {
            amount: TxItemAmounts {
                amount,
                fee: self.cfg.consensus.input_fee,
            },
            pub_key: input.note.nonce,
        })
    }

    pub async fn process_output(
        &self,
        dbtx: &WriteTx,
        output: &MintOutput,
        outpoint: OutPoint,
    ) -> Result<TxItemAmounts, MintOutputError> {
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

        let signature = self
            .cfg
            .private
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

        let amount = output.amount();

        Ok(TxItemAmounts {
            amount,
            fee: self.cfg.consensus.output_fee,
        })
    }

    pub async fn audit(&self, dbtx: &WriteTx) -> i64 {
        dbtx.iter(&IssuanceCounterTable, |r| {
            r.map(|(denomination, count)| -((denomination.amount().msat * count) as i64))
                .sum()
        })
    }

    pub async fn handle_api(&self, method: MintMethod) -> Result<Vec<u8>, String> {
        match method {
            MintMethod::SignatureShares(req) => handler_async!(signature_shares, self, req).await,
            MintMethod::SignatureSharesRestore(req) => {
                handler!(signature_shares_restore, self, req).await
            }
            MintMethod::SpendState(req) => handler!(spend_state, self, req).await,
            MintMethod::IssuanceState(req) => handler!(issuance_state, self, req).await,
        }
    }
}
