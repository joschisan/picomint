//! Traits that need to be implemented by the user.

mod dataio;
mod network;
mod tasks;

pub use aleph_bft_crypto::{
    IncompleteMultisignatureError, Index, Indexed, Keychain, Multisigned, NodeMap, NumPeers,
    PartialMultisignature, PartiallyMultisigned, PeerId, Signable, Signature, SignatureError,
    Signed, UncheckedSigned,
};
pub use dataio::{DataProvider, FinalizationHandler, OrderedUnit, UnitFinalizationHandler};
pub use network::{Network, Recipient};
pub use tasks::{SpawnHandle, TaskHandle};

use bitcoin_hashes::{sha256, Hash as BitcoinHash};
use picomint_encoding::{Decodable, Encodable};
use std::{fmt::Debug, hash::Hash as StdHash};

/// Data type that we want to order.
pub trait Data:
    Eq + Clone + Send + Sync + Debug + StdHash + Encodable + Decodable + 'static
{
}

impl<T> Data for T where
    T: Eq + Clone + Send + Sync + Debug + StdHash + Encodable + Decodable + 'static
{
}

/// Hash of a unit or other consensus message — sha256 of the consensus encoding.
pub type UnitHash = [u8; 32];

/// Hash arbitrary bytes with sha256.
pub fn hash(s: &[u8]) -> UnitHash {
    <sha256::Hash as BitcoinHash>::hash(s).to_byte_array()
}

/// The number of a session for which the consensus is run.
pub type SessionId = u64;

/// An asynchronous round of the protocol.
pub type Round = u16;
