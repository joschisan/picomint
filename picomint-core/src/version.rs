//! Consensus version of the mint, and the vote that advances it.

use derive_more::Display;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// Which revision of the consensus rules the mint runs.
///
/// One version covers everything: picomint is a single binary over a static
/// module set, so there is nothing for a per-module version to say that this
/// one does not.
///
/// The major/minor split is about clients, not guardians. For guardians
/// every bump is breaking — an unknown discriminant is a hard decode error,
/// never a skip, so there is no such thing as a change an older guardian can
/// run through. Clients are different: a rule change confined to the
/// guardian side is a minor bump they can ignore, while a change to what
/// clients see is a major bump they must support. Votes compare
/// lexicographically via the field order below.
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    Display,
)]
#[display("{major}.{minor}")]
pub struct ConsensusVersion {
    /// Incremented for changes clients must support.
    pub major: u8,
    /// Incremented for changes clients can ignore.
    pub minor: u8,
}

/// Highest consensus version this binary can run.
///
/// Each guardian votes for this and nothing else, so upgrading the binary is
/// the whole of casting a vote. Once a threshold has voted the mint
/// switches over and guardians still on an older binary halt, since they
/// cannot apply rules they do not have.
///
/// Also what a mint created by this binary starts at, recorded as
/// [`ConsensusConfig::default_version`], so a mint only ever votes to
/// climb past the version it was born with. Bumping this therefore does two
/// things at once: it makes running mints vote their way up, and it
/// makes new ones start at the top with nothing to vote about.
///
/// [`ConsensusConfig::default_version`]: crate::config::ConsensusConfig::default_version
pub const CONSENSUS_VERSION: ConsensusVersion = ConsensusVersion { major: 1, minor: 0 };
