//! Minimal AlephBFT-style atomic broadcast with consistent-broadcast unit dissemination.
//!
//! Each unit at coordinate `(round, creator)` is signed by its creator and
//! co-signed by other peers via gossip. A unit is *confirmed* once it has
//! collected `threshold` distinct co-signatures. A unit is only accepted into
//! a peer's graph if all of its parents are already in the graph and confirmed.
//! Round R+1 unit creation is triggered when the local graph has at least
//! `threshold` confirmed round-R units. Round 0 is the DAG's root row: each
//! peer creates its own round-0 unit with an empty parent set, disseminates
//! and co-signs it like any other.

mod backup;
mod data_provider;
mod engine;
mod extender;
mod graph;
mod keychain;
mod network;
mod unit;

pub use backup::{Backup, DynBackup, NoopBackup};
pub use data_provider::DataProvider;
pub use engine::run;
pub use graph::{Entry, Graph, InsertOutcome, SigOutcome};
pub use keychain::Keychain;
pub use network::{DynNetwork, INetwork, Message, MockChannel, Recipient};
pub use unit::{Round, Unit, UnitData, UnitHash};
