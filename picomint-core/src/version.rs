//! Consensus version of the federation, and the vote that advances it.

use derive_more::Display;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// Which revision of the consensus rules the federation runs.
///
/// One version covers everything: picomint is a single binary over a static
/// module set, so there is nothing for a per-module version to say that this
/// one does not.
///
/// A single counter rather than a major/minor pair. A minor bump elsewhere
/// means "adds wire variants that older peers ignore", which this encoding
/// cannot express — an unknown discriminant is a hard decode error, never a
/// skip. Every rule change is therefore breaking, and one monotonic number
/// says all there is to say about it.
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
pub struct ConsensusVersion(pub u32);

picomint_redb::consensus_value!(ConsensusVersion);

/// Highest consensus version this binary can run.
///
/// Each guardian votes for this and nothing else, so upgrading the binary is
/// the whole of casting a vote. Once a threshold has voted the federation
/// switches over and guardians still on an older binary halt, since they
/// cannot apply rules they do not have.
///
/// Also what a federation created by this binary starts at, recorded as
/// [`ConsensusConfig::default_version`], so a federation only ever votes to
/// climb past the version it was born with. Bumping this therefore does two
/// things at once: it makes running federations vote their way up, and it
/// makes new ones start at the top with nothing to vote about.
///
/// [`ConsensusConfig::default_version`]: crate::config::ConsensusConfig::default_version
pub const CONSENSUS_VERSION: ConsensusVersion = ConsensusVersion(0);
