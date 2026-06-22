use picomint_core::ln::contracts;
use picomint_core::ln::gateway::GatewayPk;
use picomint_core::{OutPoint, PeerId};
use picomint_redb::table;
use tpe;

table!(
    BlockCountVoteTable,
    PeerId => u64,
    "ln-block-count-vote",
);

table!(
    IncomingContractTable,
    OutPoint => contracts::IncomingContract,
    "ln-incoming-contract",
);

table!(
    OutgoingContractTable,
    OutPoint => contracts::OutgoingContract,
    "ln-outgoing-contract",
);

table!(
    DecryptionKeyShareTable,
    OutPoint => tpe::DecryptionKeyShare,
    "ln-decryption-key-share",
);

table!(
    PreimageTable,
    OutPoint => [u8; 32],
    "ln-preimage",
);

table!(
    GatewayTable,
    GatewayPk => (),
    "ln-gateway-pk",
);

// Incoming contracts are indexed in three ways:
// 1) A sequential stream: `stream_index (u64)` -> `(OutPoint, IncomingContract)`
//    for efficient streaming reads via range queries on
//    `IncomingContractStreamTable`.
// 2) A monotonically-increasing index (`IncomingContractStreamIndexTable` -> u64)
//    that stores the next stream index to assign, used to wait for new incoming
//    contracts.
// 3) A reverse lookup `OutPoint` -> `stream_index` via
//    `IncomingContractIndexTable`, used to remove a contract from the stream once
//    it has been spent.
table!(
    IncomingContractStreamIndexTable,
    () => u64,
    "ln-incoming-contract-stream-index",
);

table!(
    IncomingContractStreamTable,
    u64 => (OutPoint, contracts::IncomingContract),
    "ln-incoming-contract-stream",
);

table!(
    IncomingContractIndexTable,
    OutPoint => u64,
    "ln-incoming-contract-index",
);
