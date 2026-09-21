use picomint_core::config::MintId;
use picomint_core::swap::broker::BrokerPk;
use picomint_redb::table;

table!(
    ReceiveContractStreamCursorTable,
    MintId => u64,
    "swap-receive-contract-stream-cursor",
);

// The mint's announced broker pks, mirrored to disk by `update_broker_pks`
// and probed straight away on a cold start, so `swap_brokers` fills without
// waiting on the threshold-consensus broker query. The probed `BrokerInfo`
// stays in memory, never persisted.
table!(
    BrokerPkTable,
    (MintId, BrokerPk) => (),
    "swap-broker-pk",
);
