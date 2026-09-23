use bitcoin::hashes::sha256;
use picomint_core::OutPoint;
use picomint_core::lightning::contracts;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_redb::table;

table!(
    IncomingContractTable,
    OutPoint => contracts::IncomingContract,
    "lightning-incoming-contract",
);

table!(
    OutgoingContractTable,
    OutPoint => contracts::OutgoingContract,
    "lightning-outgoing-contract",
);

table!(
    PreimageTable,
    OutPoint => [u8; 32],
    "lightning-preimage",
);

// Every incoming contract ever funded, by the payment hash of the
// invoice it paid, so a payer can ask the mint whether the recipient was
// paid and take the preimage as proof. Never pruned: a claim spends the
// contract, not the fact of the payment.
table!(
    IncomingPaymentTable,
    sha256::Hash => [u8; 32],
    "lightning-incoming-payment",
);

// The value is an operator-chosen display name; it is node-local and
// never served to clients, only the set of keys is consensus-relevant.
table!(
    GatewayTable,
    GatewayPk => String,
    "lightning-gateway",
);

// Incoming contracts are indexed in three ways:
// 1) A sequential stream: `stream_index (u64)` -> `(OutPoint, IncomingContract)`
//    for efficient streaming reads via range queries on
//    `IncomingContractStreamTable`; the stream is only ever read by clients
//    hunting for their own contracts.
// 2) A monotonically-increasing index (`IncomingContractStreamNextIndexTable` -> u64)
//    that stores the next stream index to assign, used to wait for new incoming
//    contracts.
// 3) A reverse lookup `OutPoint` -> `stream_index` via
//    `IncomingContractIndexTable`, used to remove a contract from the stream once
//    it has been spent.
table!(
    IncomingContractStreamNextIndexTable,
    () => u64,
    "lightning-incoming-contract-stream-next-index",
);

table!(
    IncomingContractStreamTable,
    u64 => (OutPoint, contracts::IncomingContract),
    "lightning-incoming-contract-stream",
);

table!(
    IncomingContractIndexTable,
    OutPoint => u64,
    "lightning-incoming-contract-index",
);
