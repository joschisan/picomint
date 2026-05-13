use picomint_core::OutPoint;
use picomint_core::mint::{Denomination, RecoveryItem};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
use tbs::{BlindedMessage, BlindedSignatureShare};

/// Newtype wrapper used as the key of [`NoteNonceTable`] so we can give it a redb
/// `Key` impl locally (foreign `XOnlyPublicKey` can't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteNonceKey(pub XOnlyPublicKey);

picomint_redb::consensus_key!(NoteNonceKey);

table!(
    NoteNonceTable,
    NoteNonceKey => (),
    "mint-note-nonce",
);

table!(
    BlindedSignatureShareTable,
    OutPoint => BlindedSignatureShare,
    "mint-blinded-signature-share",
);

table!(
    BlindedSignatureShareRecoveryTable,
    BlindedMessage => BlindedSignatureShare,
    "mint-blinded-signature-share-recovery",
);

table!(
    IssuanceCounterTable,
    Denomination => u64,
    "mint-issuance-counter",
);

table!(
    RecoveryItemTable,
    u64 => RecoveryItem,
    "mint-recovery-item",
);
