use picomint_core::config::MintId;
use picomint_core::core::Account;
use picomint_redb::table;

// How far the scanner has walked the mint's output stream. The stream
// is mint-wide and one sweep serves every account, so this is a single
// cursor rather than one per account.
table!(
    NextOutputIndexTable,
    MintId => u64,
    "onchain-next-output-index",
);

// Address indices whose derived script passes `is_potential_receive`, split
// by the key's leading [`MintId`] and [`Account`]. Iterating a
// mint's prefix yields `(account, index)` pairs — exactly what the
// scanner's address map wants.
table!(
    ValidAddressIndexTable,
    (MintId, Account, u64) => (),
    "onchain-valid-address-index",
);
