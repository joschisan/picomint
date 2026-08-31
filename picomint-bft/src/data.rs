use std::ops::ControlFlow;

use async_trait::async_trait;
use picomint_core::PeerId;
use picomint_sqlite::WriteTx;

use crate::unit::{Round, UnitData};

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

/// Sink for the ordered items the extender commits. Called inline, inside
/// the write transaction that installs the unit whose arrival decided the
/// item's position — so a consumer's writes and the DAG state that caused
/// them commit atomically, and a crash can never leave one without the
/// other. That atomicity is what lets the engine restart without
/// redelivering: see [`crate::engine::Engine::run`].
///
/// The consumer sees each item exactly once per process lifetime, in the
/// agreed total order. Its writes for a rejected item must be confined to
/// the passed tx (e.g. scoped in a savepoint) — the engine commits the tx
/// regardless of what the consumer decides.
#[async_trait]
pub trait ItemConsumer<D: UnitData>: Send + 'static {
    /// Process one ordered item. `Break` stops delivery for the rest of
    /// the engine's lifetime — the session is full; the engine keeps
    /// growing and serving the DAG so lagging peers can still catch up.
    async fn process(
        &mut self,
        dbtx: &WriteTx,
        round: Round,
        creator: PeerId,
        item: D,
    ) -> ControlFlow<()>;
}
