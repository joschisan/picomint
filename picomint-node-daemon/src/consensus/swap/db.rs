use picomint_core::OutPoint;
use picomint_core::swap::broker::BrokerPk;
use picomint_core::swap::{ReceiveContract, ReceiveContractSummary, SendContract};
use picomint_redb::table;
use tbs::BlindedSignatureShare;

table!(
    SendContractTable,
    OutPoint => SendContract,
    "swap-send-contract",
);

table!(
    ReceiveContractTable,
    OutPoint => ReceiveContract,
    "swap-receive-contract",
);

// This node's attestation share for every receive contract it ever
// funded, signed as the contract is processed and kept for good: it has to
// outlive the contract's claim, since the broker holding the matching send
// contract may come for it after the recipient has taken the funds.
table!(
    AttestationShareTable,
    OutPoint => BlindedSignatureShare,
    "swap-attestation-share",
);

// The value is an operator-chosen display name; it is node-local and
// never served to clients, only the set of keys is consensus-relevant.
table!(
    BrokerTable,
    BrokerPk => String,
    "swap-broker",
);

// Receive contracts are streamed to recipients the way incoming lightning
// contracts are: a sequential stream of summaries, the next index to
// assign, and a reverse lookup to drop an entry once the contract is spent.
table!(
    ReceiveContractStreamNextIndexTable,
    () => u64,
    "swap-receive-contract-stream-next-index",
);

table!(
    ReceiveContractStreamTable,
    u64 => ReceiveContractSummary,
    "swap-receive-contract-stream",
);

table!(
    ReceiveContractIndexTable,
    OutPoint => u64,
    "swap-receive-contract-index",
);
