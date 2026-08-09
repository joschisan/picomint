use std::collections::BTreeMap;

use crate::{Amount, PeerId};
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tss::{AggregatePublicKey, PublicKeyShare, SecretKeyShare};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    pub private: WalletConfigPrivate,
    pub consensus: WalletConfigConsensus,
}

#[derive(Clone, Debug, Serialize, Deserialize, Encodable, Decodable)]
pub struct WalletConfigPrivate {
    pub sks: SecretKeyShare,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct WalletConfigConsensus {
    /// The aggregate public key of the federation's taproot wallet
    pub agg_pk: AggregatePublicKey,
    /// The public key shares of the guardians
    pub pks: BTreeMap<PeerId, PublicKeyShare>,
    /// Total vbytes of a pegout bitcoin transaction
    pub send_tx_vbytes: u64,
    /// Total vbytes of a pegin bitcoin transaction
    pub receive_tx_vbytes: u64,
    /// The minimum feerate doubles for each pending transaction in the stack,
    /// protecting against catastrophic feerate estimation errors
    pub feerate_base: u64,
    /// The minimum amount a user can send on chain
    pub dust_limit: bitcoin::Amount,
    /// Fee charged per wallet input
    pub input_fee: Amount,
    /// Fee charged per wallet output
    pub output_fee: Amount,
}

/// Converts weight to virtual bytes, defined in [BIP-141] as weight / 4
/// (rounded up to the next integer).
///
/// [BIP-141]: https://github.com/bitcoin/bips/blob/master/bip-0141.mediawiki#transaction-size-calculations
fn weight_to_vbytes(weight: u64) -> u64 {
    weight.div_ceil(bitcoin::constants::WITNESS_SCALE_FACTOR as u64)
}

impl WalletConfigConsensus {
    /// A taproot key spend has a constant witness of one 64-byte BIP340
    /// signature, so the transaction vbytes are independent of the number of
    /// guardians: 154 vbytes for a send and 169 vbytes for a receive.
    pub fn new(agg_pk: AggregatePublicKey, pks: BTreeMap<PeerId, PublicKeyShare>) -> Self {
        let tx_overhead_weight = 4 * 4 // nVersion
            + 1 // SegWit marker
            + 1 // SegWit flag
            + 4 // up to 2 inputs
            + 4 // up to 2 outputs
            + 4 * 4; // nLockTime

        let keyspend_witness_weight = 1 // witness stack item count
            + 1 // signature length prefix
            + 64; // BIP340 signature

        let change_input_weight = 32 * 4 // txid
            + 4 * 4 // vout
            + 4 // Script length
            + 4 * 4 // nSequence
            + keyspend_witness_weight;

        let change_output_weight = 8 * 4 // nValue
            + 4 // scriptPubKey length
            + 34 * 4; // scriptPubKey

        let destination_output_weight = 8 * 4 // nValue
            + 4 // scriptPubKey length
            + 34 * 4; // scriptPubKey

        Self {
            agg_pk,
            pks,
            send_tx_vbytes: weight_to_vbytes(
                tx_overhead_weight
                    + change_input_weight
                    + change_output_weight
                    + destination_output_weight,
            ),
            receive_tx_vbytes: weight_to_vbytes(
                tx_overhead_weight
                    + change_input_weight
                    + change_input_weight
                    + change_output_weight,
            ),
            // This is intentionally lower than the 1 sat/vB minimum feerate
            // vote floor. This allows for at least three pending transactions
            // which only pay the consensus feerate before the exponential
            // doubling kicks in.
            feerate_base: 250,
            dust_limit: bitcoin::Amount::from_sat(10_000),
            input_fee: crate::Amount::from_sat(10),
            output_fee: crate::Amount::from_sat(10),
        }
    }
}
