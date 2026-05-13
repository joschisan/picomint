client_table!(
    NextOutputIndexTable,
    () => u64,
    "wallet-next-output-index",
);

client_table!(
    ValidAddressIndexTable,
    u64 => (),
    "wallet-valid-address-index",
);
