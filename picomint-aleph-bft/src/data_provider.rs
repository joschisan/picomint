use async_trait::async_trait;

use crate::unit::UnitData;

/// Source of unit payloads. The engine calls `get_data` once per unit it
/// creates; the returned `Vec<D>` becomes the unit's `data` field. Empty
/// vec is fine — the unit will simply carry no items.
///
/// Mirrors the upstream `aleph_bft::DataProvider<D>` shape, with the data
/// type widened from a single opaque blob to `Vec<D>` so the engine can
/// emit individual `D` items in finalization order downstream.
#[async_trait]
pub trait DataProvider<D: UnitData>: Send + 'static {
    /// Produce the next unit's payload.
    async fn get_data(&mut self) -> Vec<D>;
}
