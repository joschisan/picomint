use bitcoin::hashes::sha256;
use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};

use serde::{Deserialize, Serialize};

/// Maximum number of guardians embedded in a single [`LnurlRequest`]. The
/// LNURL daemon probes them in parallel, so capping keeps the fan-out
/// bounded and the encoded payload small. A client embeds `f + 1`, which
/// is 8 for the largest supported mint of `3f + 1 = 22`.
pub const MAX_GUARDIANS_PER_LNURL: usize = 8;

/// Payload embedded in the LNURL a client hands out.
///
/// Names no mint id and no gateways. `info` is the hash of the
/// [`MintInfoResponse`] the daemon expects back, which is what lets a
/// single guardian answer: it can refuse, but a forged node set will not
/// hash. Everything downstream — the tpe aggregate key, the announced
/// gateways — is then threshold-read from the node set that response carries.
///
/// Every field is immutable for the mint's lifetime, so the string
/// stays valid however often the gateway set turns over.
///
/// [`MintInfoResponse`]: crate::methods::MintInfoResponse
#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct LnurlRequest {
    pub recipient: secp256k1::PublicKey,
    pub guardians: Vec<iroh_base::PublicKey>,
    pub info: sha256::Hash,
}
