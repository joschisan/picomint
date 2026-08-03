//! Minimal BFT atomic broadcast over a DAG of creator-signed units.
//!
//! Each unit at coordinate `(round, creator)` is signed by its creator and
//! broadcast to all peers. A unit is *extended* once it is locally stored
//! and all of its parents are extended. Round R+1 unit creation is
//! triggered when the local graph has at least `threshold` extended
//! round-R units. Round 0 is the DAG's root row: each peer creates its own
//! round-0 unit with an empty parent set and disseminates it like any
//! other.
//!
//! Equivocation tolerance is not yet implemented: peers store the first
//! valid body they see per slot, and the ordering rule assumes slot
//! uniqueness. The fork-tolerant (Mysticeti-style) commit rule that
//! replaces the removed cosig layer lands in follow-up commits.
//!
//! Unit creation is work-gated: a peer only builds a unit that carries
//! items, or that keeps the DAG growing while an earlier unit of its own
//! still awaits ordering. A peer with no items inflight creates nothing
//! and idles until its `DataProvider` yields a fresh item.

mod data;
mod engine;
mod extender;
mod keychain;
mod network;
mod unit;

pub use data::DataProvider;
pub use engine::Engine;
pub use keychain::Keychain;
pub use network::{DynNetwork, INetwork, Message, Recipient};
pub use unit::{Round, SignedUnit, Unit};
