use picomint_core::OutPoint;
use picomint_core::ecash::Denomination;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_redb::table;
use tbs::{BlindedMessage, BlindedSignatureShare};

table!(
    NoteNonceTable,
    XOnlyPublicKey => (),
    "ecash-note-nonce",
);

table!(
    /// Every blinded nonce the mint has ever signed, and the denomination it
    /// signed it under. Mirrors [`NoteNonceTable`] on the output side: one
    /// guards against spending a note twice, the other against signing a
    /// nonce twice.
    ///
    /// That uniqueness is what makes the value well defined — a blinded
    /// message is rejected on its second appearance regardless of
    /// denomination, so it can never have been signed under two. A restoring
    /// client derives nonces from a counter alone and cannot recover the
    /// denomination from its seed, so this is where the answer comes from.
    /// The share itself stays in [`BlindedSignatureShareRestoreTable`], which
    /// a membership probe still has no reason to read.
    BlindedNonceTable,
    BlindedMessage => Denomination,
    "ecash-blinded-nonce",
);

table!(
    BlindedSignatureShareTable,
    OutPoint => BlindedSignatureShare,
    "ecash-blinded-signature-share",
);

table!(
    BlindedSignatureShareRestoreTable,
    BlindedMessage => BlindedSignatureShare,
    "ecash-blinded-signature-share-restore",
);

table!(
    IssuanceDerivationCounterTable,
    Denomination => u64,
    "ecash-issuance-counter",
);
