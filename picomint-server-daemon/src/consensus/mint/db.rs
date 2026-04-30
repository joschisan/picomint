use picomint_core::OutPoint;
use picomint_core::mint::{Denomination, RecoveryItem};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
use tbs::{BlindedMessage, BlindedSignatureShare};

/// Newtype wrapper used as the key of [`NOTE_NONCE`] so we can give it a redb
/// `Key` impl locally (foreign `XOnlyPublicKey` can't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteNonceKey(pub XOnlyPublicKey);

picomint_redb::consensus_key!(NoteNonceKey);

table!(
    NOTE_NONCE,
    NoteNonceKey => (),
    "mint-note-nonce",
);

table!(
    BLINDED_SIGNATURE_SHARE,
    OutPoint => BlindedSignatureShare,
    "mint-blinded-signature-share",
);

table!(
    BLINDED_SIGNATURE_SHARE_RECOVERY,
    BlindedMessage => BlindedSignatureShare,
    "mint-blinded-signature-share-recovery",
);

table!(
    ISSUANCE_COUNTER,
    Denomination => u64,
    "mint-issuance-counter",
);

table!(
    RECOVERY_ITEM,
    u64 => RecoveryItem,
    "mint-recovery-item",
);
