//! Per-module state machine executor.
//!
//! Two functions over the shared [`ClientContext`]: [`resume`] restarts a
//! table's persisted state machines at federation bring-up, and
//! [`add_state_machine_dbtx`] lands a new one atomically with the caller's
//! writes. Tables are shared across federations, so both scope by the
//! context federation's slice of the key space; active states are keyed by
//! `(federation, SmId)` and driven in a typed reactor loop.
//!
//! Each driver iteration: wait for [`StateMachine::trigger`] to resolve,
//! then apply [`StateMachine::transition`] atomically in a DB tx. A
//! transition returning `None` terminates the SM — the executor removes
//! the row and the driver exits. Inactive state history is not retained.

use std::fmt::Debug;
use std::future::Future;

use crate::context::ClientContext;
use picomint_core::config::FederationId;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{DbRead, Prefix, Table, WriteTx};

/// Random opaque identifier assigned by the executor when a state
/// machine is first inserted. Used as the table key; the state machine
/// struct is the stored value.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Encodable, Decodable)]
pub struct SmId([u8; 16]);

impl SmId {
    fn random() -> Self {
        Self(rand::random())
    }
}

/// A persistent state machine driven by [`resume`] and the drive loop.
///
/// States with multiple concurrent reasons-to-transition fold them into
/// [`Self::Outcome`] via `tokio::select!` inside [`Self::trigger`]. The
/// drive loop hands the resolved outcome to
/// [`Self::transition`], which runs atomically in a write tx and either
/// produces the next state or `None` to terminate.
pub(crate) trait StateMachine:
    Debug + Clone + Encodable + Decodable + Send + Sync + 'static
{
    /// Value produced by [`Self::trigger`] and consumed by
    /// [`Self::transition`]. For SMs with multi-variant state this is
    /// usually a sum type.
    type Outcome: Send + 'static;

    /// Future whose resolution drives the next transition. Awaited by the
    /// driver with both `self` and `ctx` still live, so impls can borrow.
    ///
    /// Written as explicit RPITIT (not `async fn`) to require the returned
    /// future is `Send` — the executor spawns the drive loop on the
    /// multi-threaded runtime. Impls may still use `async fn`; the compiler
    /// proves the resulting future matches the `Send` bound.
    fn trigger<'a>(
        &'a self,
        ctx: &'a ClientContext,
    ) -> impl Future<Output = Self::Outcome> + Send + 'a;

    /// Apply `outcome` atomically inside `dbtx`, producing the next state.
    /// `None` terminates the state machine.
    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self>;
}

/// Resume every state machine the context's federation persisted in `table`
/// from a previous run. Called exactly once per federation bring-up — a
/// second call would double-drive every active state machine.
pub(crate) fn resume<S, T>(ctx: &ClientContext, table: T)
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
    FederationId: Prefix<T>,
{
    let active: Vec<(SmId, S)> = ctx.db.begin_read().prefix(&table, &ctx.federation, |r| {
        r.map(|entry| (entry.0.1, entry.1)).collect()
    });

    for (id, state) in active {
        spawn_drive(ctx.clone(), table, id, state);
    }
}

/// Atomically insert `state` as a new active state machine under a
/// freshly-generated [`SmId`]. A driver task is spawned for it when the DB
/// transaction commits.
pub(crate) fn add_state_machine_dbtx<S, T>(ctx: &ClientContext, table: T, dbtx: &WriteTx, state: S)
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
{
    let id = SmId::random();
    assert!(
        dbtx.insert(&table, &(ctx.federation, id), &state).is_none(),
        "SmId collision"
    );

    let ctx = ctx.clone();

    dbtx.on_commit(move || {
        spawn_drive(ctx, table, id, state);
    });
}

fn spawn_drive<S, T>(ctx: ClientContext, table: T, id: SmId, state: S)
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
{
    let tg = ctx.tg.clone();

    tg.spawn(drive(ctx, table, id, state));
}

/// Drive one state machine until `transition` returns `None`. Each
/// iteration: await the trigger, then apply the transition atomically and
/// write (or delete) the state row.
async fn drive<S, T>(ctx: ClientContext, table: T, id: SmId, mut state: S)
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
{
    loop {
        let outcome = state.trigger(&ctx).await;

        let dbtx = ctx.db.begin_write();

        match state.transition(&ctx, &dbtx, outcome) {
            Some(new_state) => {
                dbtx.insert(&table, &(ctx.federation, id), &new_state);
                dbtx.commit();
                state = new_state;
            }
            None => {
                dbtx.remove(&table, &(ctx.federation, id));
                dbtx.commit();
                return;
            }
        }
    }
}
