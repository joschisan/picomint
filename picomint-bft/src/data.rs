use async_trait::async_trait;

use crate::unit::UnitData;

/// Source of unit payloads. The engine calls `get_data` once per unit it
/// creates; the returned `Vec` becomes the unit's `data` field. Empty
/// vec is fine — the unit will simply carry no items.
///
/// `D` is the payload item type — what the caller wants to atomically
/// broadcast through bft. See [`UnitData`] for the bound bundle.
#[async_trait]
pub trait DataProvider<D: UnitData>: Send + 'static {
    /// Produce the next unit's payload.
    fn get_data(&mut self) -> Vec<D>;

    /// Resolve when a fresh item arrives, waking the engine out of
    /// quiescence. Must be cancel-safe: an item observed by a cancelled
    /// call must still be returned by a later `get_data`. Items already
    /// observed need not resolve this again — the engine re-checks
    /// `get_data` on every inbound message.
    async fn wait_for_data(&mut self);
}
