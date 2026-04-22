mod db;
mod rpc;

use std::collections::BTreeMap;

use anyhow::ensure;
use picomint_core::mint::config::{
    MintConfig, MintConfigConsensus, MintConfigPrivate, consensus_denominations,
};
use picomint_core::mint::methods::MintMethod;
use picomint_core::mint::{
    Denomination, MintConsensusItem, MintInput, MintInputError, MintOutput, MintOutputError,
    RecoveryItem, verify_note,
};
use picomint_core::module::{ApiError, InputMeta, TransactionItemAmounts};
use picomint_core::{Amount, InPoint, OutPoint, PeerId};
use picomint_encoding::Encodable;
use picomint_redb::{Database, ReadTxRef, WriteTxRef};
use tbs::{AggregatePublicKey, PublicKeyShare, derive_pk_share};
use threshold_crypto::group::Curve;

use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g2;
use crate::{handler, handler_async};

use self::db::{
    BLINDED_SIGNATURE_SHARE, BLINDED_SIGNATURE_SHARE_RECOVERY, ISSUANCE_COUNTER, NOTE_NONCE,
    NoteNonceKey, RECOVERY_ITEM,
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
            input_fee: Amount::from_msats(100),
            output_fee: Amount::from_msats(100),
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

    pub async fn note_distribution_ui(&self) -> BTreeMap<Denomination, u64> {
        self.db.begin_read().iter(&ISSUANCE_COUNTER, |r| {
            r.filter(|(_, count)| *count > 0).collect()
        })
    }
}

impl Mint {
    pub async fn consensus_proposal(&self, _dbtx: &ReadTxRef<'_>) -> Vec<MintConsensusItem> {
        Vec::new()
    }

    pub async fn process_consensus_item(
        &self,
        _dbtx: &WriteTxRef<'_>,
        consensus_item: MintConsensusItem,
        _peer_id: PeerId,
    ) -> anyhow::Result<()> {
        match consensus_item {}
    }

    pub async fn process_input(
        &self,
        dbtx: &WriteTxRef<'_>,
        input: &MintInput,
        _in_point: InPoint,
    ) -> Result<InputMeta, MintInputError> {
        let pk = self
            .cfg
            .consensus
            .tbs_agg_pks
            .get(&input.note.denomination)
            .ok_or(MintInputError::InvalidDenomination)?;

        if !verify_note(input.note, *pk) {
            return Err(MintInputError::InvalidSignature);
        }

        if dbtx
            .insert(&NOTE_NONCE, &NoteNonceKey(input.note.nonce), &())
            .is_some()
        {
            return Err(MintInputError::SpentCoin);
        }

        let new_count = dbtx
            .remove(&ISSUANCE_COUNTER, &input.note.denomination)
            .unwrap_or(0)
            .checked_sub(1)
            .expect("Failed to decrement issuance counter");

        dbtx.insert(&ISSUANCE_COUNTER, &input.note.denomination, &new_count);

        let next_index = get_recovery_count(dbtx);

        dbtx.insert(
            &RECOVERY_ITEM,
            &next_index,
            &RecoveryItem::Input {
                nonce_hash: input.note.nonce.consensus_hash(),
            },
        );

        let amount = input.note.amount();

        Ok(InputMeta {
            amount: TransactionItemAmounts {
                amount,
                fee: self.cfg.consensus.input_fee,
            },
            pub_key: input.note.nonce,
        })
    }

    pub async fn process_output(
        &self,
        dbtx: &WriteTxRef<'_>,
        output: &MintOutput,
        outpoint: OutPoint,
    ) -> Result<TransactionItemAmounts, MintOutputError> {
        let signature = self
            .cfg
            .private
            .tbs_sks
            .get(&output.denomination)
            .map(|key| tbs::sign_message(output.nonce, *key))
            .ok_or(MintOutputError::InvalidDenomination)?;

        dbtx.insert(&BLINDED_SIGNATURE_SHARE, &outpoint, &signature);

        dbtx.insert(&BLINDED_SIGNATURE_SHARE_RECOVERY, &output.nonce, &signature);

        let new_count = dbtx
            .remove(&ISSUANCE_COUNTER, &output.denomination)
            .unwrap_or(0)
            .checked_add(1)
            .expect("Failed to increment issuance counter");

        dbtx.insert(&ISSUANCE_COUNTER, &output.denomination, &new_count);

        let next_index = get_recovery_count(dbtx);

        dbtx.insert(
            &RECOVERY_ITEM,
            &next_index,
            &RecoveryItem::Output {
                denomination: output.denomination,
                nonce_hash: output.nonce.consensus_hash(),
                tweak: output.tweak,
            },
        );

        let amount = output.amount();

        Ok(TransactionItemAmounts {
            amount,
            fee: self.cfg.consensus.output_fee,
        })
    }

    pub async fn audit(&self, dbtx: &WriteTxRef<'_>) -> i64 {
        dbtx.iter(&ISSUANCE_COUNTER, |r| {
            r.map(|(denomination, count)| -((denomination.amount().msats * count) as i64))
                .sum()
        })
    }

    pub async fn handle_api(&self, method: MintMethod) -> Result<Vec<u8>, ApiError> {
        match method {
            MintMethod::SignatureShares(req) => handler_async!(signature_shares, self, req).await,
            MintMethod::SignatureSharesRecovery(req) => {
                handler!(signature_shares_recovery, self, req).await
            }
            MintMethod::RecoverySlice(req) => handler!(recovery_slice, self, req).await,
            MintMethod::RecoverySliceHash(req) => handler!(recovery_slice_hash, self, req).await,
            MintMethod::RecoveryCount(req) => handler!(recovery_count, self, req).await,
        }
    }
}

pub(crate) fn get_recovery_count(dbtx: &impl picomint_redb::DbRead) -> u64 {
    dbtx.iter(&RECOVERY_ITEM, |r| r.next_back().map_or(0, |(k, _)| k + 1))
}
