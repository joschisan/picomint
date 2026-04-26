use aleph_bft::Round;
use picomint_core::PeerId;

use super::data_provider::UnitData;

pub struct OrderedUnit {
    pub creator: PeerId,
    pub round: Round,
    pub data: Option<UnitData>,
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
    type Data = UnitData;

    fn batch_finalized(&mut self, batch: Vec<aleph_bft::OrderedUnit<Self::Data>>) {
        for unit in batch {
            // the channel is unbounded
            self.sender
                .try_send(OrderedUnit {
                    creator: super::to_peer_id(unit.creator),
                    round: unit.round,
                    data: unit.data,
                })
                .ok();
        }
    }
}
