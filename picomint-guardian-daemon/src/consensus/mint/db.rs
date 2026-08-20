use picomint_core::OutPoint;
use picomint_core::mint::Denomination;
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
    /// Every blinded nonce the mint has ever signed. Membership only — the
    /// shares live in [`BlindedSignatureShareRestoreTable`], and a restore
    /// probe has no use for them. Mirrors [`NoteNonceTable`] on the output
    /// side: one guards against spending a note twice, the other against
    /// signing a nonce twice.
    BlindedNonceTable,
    BlindedMessage => (),
    "mint-blinded-nonce",
);

table!(
    BlindedSignatureShareTable,
    OutPoint => BlindedSignatureShare,
    "mint-blinded-signature-share",
);

table!(
    BlindedSignatureShareRestoreTable,
    BlindedMessage => BlindedSignatureShare,
    "mint-blinded-signature-share-restore",
);

table!(
    IssuanceCounterTable,
    Denomination => u64,
    "mint-issuance-counter",
);
