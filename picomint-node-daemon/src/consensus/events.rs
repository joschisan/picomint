//! The core events: the consensus clock, height, version and session.

use picomint_core::sql::SqlRow;
use picomint_core::version::ConsensusVersion;
use serde::{Deserialize, Serialize};

use crate::consensus::eventlog::Event;

/// The consensus block height moved: a threshold of nodes now vote for
/// at least this height. Everything logged after this and before the
/// next one happened at this height.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct HeightEvent {
    pub height: u32,
}

impl Event for HeightEvent {
    const KIND: &'static str = "height";
}

/// The consensus version moved.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct VersionEvent {
    pub version: ConsensusVersion,
}

impl Event for VersionEvent {
    const KIND: &'static str = "version";
}

/// A session closed; everything logged after this and before the next
/// one belongs to the following session.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct SessionEvent {
    pub session: u32,
}

impl Event for SessionEvent {
    const KIND: &'static str = "session";
}
