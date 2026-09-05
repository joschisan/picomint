//! Core module system types shared between the server and client sides.
pub mod audit;

use serde::{Deserialize, Serialize};

use crate::ecash::methods::EcashMethod;
use crate::lightning::methods::LightningMethod;
use crate::methods::CoreMethod;
use crate::onchain::methods::OnchainMethod;
use picomint_encoding::{Decodable, Encodable};

/// The wire method dispatched to a node over iroh. Each variant carries
/// the concrete request for its module; the response type is determined by
/// the variant the client sent.
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum Method {
    Core(CoreMethod),
    Ecash(EcashMethod),
    Onchain(OnchainMethod),
    Lightning(LightningMethod),
}

impl Method {
    /// Short stable name for log lines. Requests carry whole transactions,
    /// so their `Debug` output is far too big to log on the hot path.
    pub fn name(&self) -> &'static str {
        match self {
            Method::Core(CoreMethod::Config(_)) => "core/config",
            Method::Core(CoreMethod::SubmitTx(_)) => "core/submit-tx",
            Method::Core(CoreMethod::BlockCount(_)) => "core/block-count",
            Method::Core(CoreMethod::Liveness(_)) => "core/liveness",
            Method::Core(CoreMethod::ExpiryStatus(_)) => "core/expiry-status",
            Method::Core(CoreMethod::MintInfo(_)) => "core/mint-info",
            Method::Ecash(EcashMethod::SignatureShares(_)) => "ecash/signature-shares",
            Method::Ecash(EcashMethod::SignatureSharesRestore(_)) => {
                "ecash/signature-shares-restore"
            }
            Method::Ecash(EcashMethod::SpendState(_)) => "ecash/spend-state",
            Method::Ecash(EcashMethod::IssuanceState(_)) => "ecash/issuance-state",
            Method::Onchain(OnchainMethod::ConsensusFeerate(_)) => "onchain/consensus-feerate",
            Method::Onchain(OnchainMethod::MintUtxo(_)) => "onchain/mint-utxo",
            Method::Onchain(OnchainMethod::SendFee(_)) => "onchain/send-fee",
            Method::Onchain(OnchainMethod::ReceiveFee(_)) => "onchain/receive-fee",
            Method::Onchain(OnchainMethod::TxId(_)) => "onchain/txid",
            Method::Onchain(OnchainMethod::OutputInfoSlice(_)) => "onchain/output-info-slice",
            Method::Onchain(OnchainMethod::PendingTxChain(_)) => "onchain/pending-tx-chain",
            Method::Onchain(OnchainMethod::TxChain(_)) => "onchain/tx-chain",
            Method::Lightning(LightningMethod::AwaitPreimage(_)) => "lightning/await-preimage",
            Method::Lightning(LightningMethod::DecryptionKeyShare(_)) => {
                "lightning/decryption-key-share"
            }
            Method::Lightning(LightningMethod::OutgoingContractExpiry(_)) => {
                "lightning/outgoing-contract-expiry"
            }
            Method::Lightning(LightningMethod::AwaitIncomingContracts(_)) => {
                "lightning/await-incoming-contracts"
            }
            Method::Lightning(LightningMethod::Gateways(_)) => "lightning/gateways",
            Method::Lightning(LightningMethod::TpeAggregatePk(_)) => "lightning/tpe-aggregate-pk",
        }
    }
}

/// Authentication secret used to verify node admin API requests.
///
/// The inner value is private to prevent timing leaks via direct comparison.
/// Use [`Self::verify`] for authentication checks. No `Debug` impl — the
/// plaintext must never end up in a log. [`Self::as_str`] is a temporary
/// escape hatch for I/O that still needs the plaintext value and should be
/// removed once passwords are hashed at rest.
#[derive(Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct ApiAuth(String);

impl ApiAuth {
    pub fn new(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn verify(&self, password: &str) -> bool {
        use subtle::ConstantTimeEq as _;
        bool::from(self.0.as_bytes().ct_eq(password.as_bytes()))
    }
}
