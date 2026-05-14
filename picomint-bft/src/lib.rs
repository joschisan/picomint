//! Minimal BFT atomic broadcast with consistent-broadcast unit dissemination.
//!
//! Each unit at coordinate `(round, creator)` is signed by its creator and
//! co-signed by other peers via gossip. A unit is *confirmed* once it has
//! collected `threshold` distinct co-signatures. A unit is only accepted into
//! a peer's graph if all of its parents are already in the graph and confirmed.
//! Round R+1 unit creation is triggered when the local graph has at least
//! `threshold` confirmed round-R units. Round 0 is the DAG's root row: each
//! peer creates its own round-0 unit with an empty parent set, disseminates
//! and co-signs it like any other.

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
pub use unit::{Cosig, Round, Unit};
