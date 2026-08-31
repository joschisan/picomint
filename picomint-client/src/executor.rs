//! Per-module state machine executor.
//!
//! Each module (and the client-level tx submission) owns a single
//! [`ModuleExecutor<S, T>`] parameterised by its state type and the
//! [`Table`] that persists it. Tables are shared across federations, so the
//! executor scopes every op by its federation's slice of the key space.
//! Active states are keyed by `(federation, SmId)` and driven in a typed
//! reactor loop.
//!
//! Each driver iteration: wait for [`StateMachine::trigger`] to resolve,
//! then apply [`StateMachine::transition`] atomically in a DB tx. A
//! transition returning `None` terminates the SM — the executor removes
//! the row and the driver exits. Inactive state history is not retained.

use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

use crate::task::TaskGroup;
use picomint_core::config::FederationId;
use picomint_encoding::{Decodable, Encodable};
use picomint_sqlite::{Database, DbRead, Prefix, Table, WriteTx};

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

/// A persistent state machine driven by a [`ModuleExecutor`].
///
/// States with multiple concurrent reasons-to-transition fold them into
/// [`Self::Outcome`] via `tokio::select!` inside [`Self::trigger`]. The
/// owning [`ModuleExecutor`] hands the resolved outcome to
/// [`Self::transition`], which runs atomically in a write tx and either
/// produces the next state or `None` to terminate.
pub trait StateMachine: Debug + Clone + Encodable + Decodable + Send + Sync + 'static {
    /// Per-module context handed to `trigger` and `transition`.
    type Context: Clone + Send + Sync + 'static;

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
        ctx: &'a Self::Context,
    ) -> impl Future<Output = Self::Outcome> + Send + 'a;

    /// Apply `outcome` atomically inside `dbtx`, producing the next state.
    /// `None` terminates the state machine.
    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self>;
}

/// Per-module reactor driving state machines of type `S` persisted in `T`
/// under `federation`'s key prefix. Cheaply cloneable ([`Arc`]-backed).
#[derive(Clone)]
pub struct ModuleExecutor<S: StateMachine, T> {
    inner: Arc<Inner<S, T>>,
}

struct Inner<S: StateMachine, T> {
    db: Database,
    federation: FederationId,
    table: T,
    context: S::Context,
    tg: TaskGroup,
}

impl<S, T> ModuleExecutor<S, T>
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
    FederationId: Prefix<T>,
{
    /// Resume every state machine `federation` persisted in `table` from a
    /// previous run, and drive new ones as they are added.
    pub fn new(
        db: Database,
        federation: FederationId,
        table: T,
        context: S::Context,
        tg: TaskGroup,
    ) -> Self {
        let inner = Arc::new(Inner {
            db,
            federation,
            table,
            context,
            tg,
        });

        for (id, state) in inner.get_active_states() {
            inner.clone().spawn_drive(id, state);
        }

        Self { inner }
    }

    /// Atomically insert `state` as a new active state machine under a
    /// freshly-generated [`SmId`]. A driver task is spawned for it when
    /// the DB transaction commits.
    pub fn add_state_machine_dbtx(&self, dbtx: &WriteTx, state: S) {
        let id = SmId::random();
        assert!(
            dbtx.insert(&self.inner.table, &(self.inner.federation, id), &state)
                .is_none(),
            "SmId collision"
        );

        let inner = self.inner.clone();

        dbtx.on_commit(move || {
            inner.spawn_drive(id, state);
        });
    }
}

impl<S, T> Inner<S, T>
where
    S: StateMachine,
    T: Table<Key = (FederationId, SmId), Value = S> + Copy + Send + Sync + 'static,
    FederationId: Prefix<T>,
{
    fn get_active_states(&self) -> Vec<(SmId, S)> {
        self.db
            .begin_read()
            .prefix(&self.table, &self.federation, |r| {
                r.map(|entry| (entry.0.1, entry.1)).collect()
            })
    }

    fn spawn_drive(self: Arc<Self>, id: SmId, state: S) {
        let tg = self.tg.clone();
        tg.spawn(self.drive(id, state));
    }

    /// Drive one state machine until `transition` returns `None`. Each
    /// iteration: await the trigger, then apply the transition atomically
    /// and write (or delete) the state row.
    async fn drive(self: Arc<Self>, id: SmId, mut state: S) {
        loop {
            let outcome = state.trigger(&self.context).await;

            let dbtx = self.db.begin_write();

            match state.transition(&self.context, &dbtx, outcome) {
                Some(new_state) => {
                    dbtx.insert(&self.table, &(self.federation, id), &new_state);
                    dbtx.commit();
                    state = new_state;
                }
                None => {
                    dbtx.remove(&self.table, &(self.federation, id));
                    dbtx.commit();
                    return;
                }
            }
        }
    }
}
