use crate::unit::UnitData;

/// Source of unit payloads. The engine calls `get_data` once per unit it
/// creates; the returned `Vec` becomes the unit's `data` field. Empty
/// vec is fine — the unit will simply carry no items.
///
/// `D` is the payload item type — what the caller wants to atomically
/// broadcast through bft. See [`UnitData`] for the bound bundle.
pub trait DataProvider<D: UnitData>: Send + 'static {
    /// Produce the next unit's payload.
    fn get_data(&mut self) -> Vec<D>;
}
