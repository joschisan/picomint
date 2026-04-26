use aleph_bft::Round;
use picomint_core::PeerId;
use picomint_core::transaction::ConsensusItem;

pub struct OrderedUnit {
    pub creator: PeerId,
    pub round: Round,
    pub data: Option<Vec<ConsensusItem>>,
}

pub struct FinalizationHandler {
    sender: async_channel::Sender<OrderedUnit>,
}

impl FinalizationHandler {
    pub fn new(sender: async_channel::Sender<OrderedUnit>) -> Self {
        Self { sender }
    }
}

impl aleph_bft::UnitFinalizationHandler for FinalizationHandler {
    type Data = Vec<ConsensusItem>;

    fn batch_finalized(&mut self, batch: Vec<aleph_bft::OrderedUnit<Self::Data>>) {
        for unit in batch {
            // the channel is unbounded
            self.sender
                .try_send(OrderedUnit {
                    creator: unit.creator,
                    round: unit.round,
                    data: unit.data,
                })
                .ok();
        }
    }
}
