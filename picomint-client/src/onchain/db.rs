use picomint_core::config::FederationId;
use picomint_core::core::Account;
use picomint_redb::table;

// How far the scanner has walked the federation's output stream. The stream
// is federation-wide and one sweep serves every account, so this is a single
// cursor rather than one per account.
table!(
    NextOutputIndexTable,
    FederationId => u64,
    "wallet-next-output-index",
);

// Address indices whose derived script passes `is_potential_receive`, split
// by the key's leading [`FederationId`] and [`Account`]. Iterating a
// federation's prefix yields `(account, index)` pairs — exactly what the
// scanner's address map wants.
table!(
    ValidAddressIndexTable,
    (FederationId, Account, u64) => (),
    "wallet-valid-address-index",
);
