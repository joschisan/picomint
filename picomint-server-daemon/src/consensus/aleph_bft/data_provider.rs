use std::collections::BTreeSet;

use picomint_core::TransactionId;
use picomint_core::config::ALEPH_BFT_UNIT_BYTE_LIMIT;
use picomint_core::transaction::ConsensusItem;
use picomint_encoding::Encodable;

use crate::LOG_CONSENSUS;

/// Returns true iff `items` encode to at most `ALEPH_BFT_UNIT_BYTE_LIMIT` bytes.
/// Used by the network receive path to drop oversize units sent by malicious peers.
pub fn is_valid(items: &[ConsensusItem]) -> bool {
    items.consensus_encode_to_vec().len() <= ALEPH_BFT_UNIT_BYTE_LIMIT
}

pub struct DataProvider {
    mempool_item_receiver: async_channel::Receiver<ConsensusItem>,
    submitted_transactions: BTreeSet<TransactionId>,
    leftover_item: Option<ConsensusItem>,
}

impl DataProvider {
    pub fn new(mempool_item_receiver: async_channel::Receiver<ConsensusItem>) -> Self {
        Self {
            mempool_item_receiver,
            submitted_transactions: BTreeSet::new(),
            leftover_item: None,
        }
    }
}

#[async_trait::async_trait]
impl aleph_bft::DataProvider for DataProvider {
    type Output = Vec<ConsensusItem>;

    async fn get_data(&mut self) -> Option<Vec<ConsensusItem>> {
        // 4-byte u32 BE length prefix from picomint-encoding's Vec encoding
        let mut n_bytes = 4;
        let mut items = Vec::new();

        if let Some(item) = self.leftover_item.take() {
            let n_bytes_item = item.consensus_encode_to_vec().len();

            if n_bytes_item + n_bytes <= ALEPH_BFT_UNIT_BYTE_LIMIT {
                n_bytes += n_bytes_item;
                items.push(item);
            } else {
                tracing::warn!(target: LOG_CONSENSUS, ?item, "Consensus item length is over BYTE_LIMIT");
            }
        }

        // if the channel is empty we want to return the batch immediately in order to
        // not delay the creation of our next unit, even if the batch is empty
        while let Ok(item) = self.mempool_item_receiver.try_recv() {
            if let ConsensusItem::Transaction(transaction) = &item
                && !self.submitted_transactions.insert(transaction.tx_hash())
            {
                continue;
            }

            let n_bytes_item = item.consensus_encode_to_vec().len();

            if n_bytes + n_bytes_item <= ALEPH_BFT_UNIT_BYTE_LIMIT {
                n_bytes += n_bytes_item;
                items.push(item);
            } else {
                self.leftover_item = Some(item);
                break;
            }
        }

        if items.is_empty() {
            return None;
        }

        assert!(is_valid(&items));

        Some(items)
    }
}
