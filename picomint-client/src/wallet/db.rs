use picomint_core::core::Account;

// How far the scanner has walked the federation's output stream. The stream
// is federation-wide and one sweep serves every account, so this is a single
// cursor rather than one per account.
client_table!(
    NextOutputIndexTable,
    () => u64,
    "wallet-next-output-index",
);

// Address indices whose derived script passes `is_potential_receive`, split
// by the key's leading [`Account`]. Iterating the whole table yields
// `(account, index)` pairs — exactly what the scanner's address map wants.
client_table!(
    ValidAddressIndexTable,
    (Account, u64) => (),
    "wallet-valid-address-index",
);
