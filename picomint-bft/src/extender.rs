//! Per-node order extender running a deterministic QuickAleph-style
//! virtual-voting rule (arXiv:1908.05156, Appendix A/C.2) with the
//! coin replaced by seeded pseudo-random bits — sound under the
//! benign-network model, where common votes only need to be *common*,
//! not unpredictable.
//!
//! For each round R, every extended round-R branch is a *candidate*,
//! walked in ascending hash order. Each candidate resolves to a
//! binary include/exclude decision:
//!
//! - A round-`R+1` unit **votes** 1 for candidate `B` iff its parent
//!   entry for `B`'s creator is exactly `B`'s hash.
//! - A unit above `R+1` adopts its parents' vote if they are
//!   **unanimous**, otherwise it votes the round's [`common_vote`]:
//!   fixed 1 at `R+2`, fixed 0 at `R+3`, seeded bits above. The
//!   unanimity-else-default aggregation is the fork armor: two
//!   same-round units with unanimous parent votes share an honest
//!   creator's (single-voiced) parent unit, so they cannot disagree.
//! - A unit at round `R+2` or above **decides** the common-vote value
//!   `v` of its round iff at least `2f+1` of its parents vote `v`.
//!   One deciding unit suffices: its `2f+1` `v`-voting parents
//!   intersect every same-round unit's parents in an honest creator,
//!   forcing the whole round to vote `v` and every round above to
//!   inherit `v` unanimously — the decision propagates structurally.
//!
//! The fixed 1 at `R+2` gives well-referenced candidates a
//! three-message-delay include; the fixed 0 at `R+3` kills invisible
//! candidates fast; the seeded bits from `R+4` break middle-band ties
//! within expected one extra round under random network behavior.
//!
//! The round head is the first candidate in hash order decided 1,
//! after every earlier candidate is decided 0; an undecided candidate
//! blocks the round until later rounds decide it. The walk order only
//! needs to be *common* across nodes, not unpredictable — the paper's
//! secret permutation defends latency against an adaptive network
//! adversary the benign model excludes, and a creator can grind its
//! unit hash for early position under any public order. Head election
//! waits for an extended round-`R+3` unit, which is what makes
//! walking a deterministic (rather than secret) order over the
//! *local* candidate set safe — the coverage lemma (C.10): a round-R candidate
//! outside the ancestry of any round-`R+3` unit can never gather a
//! round-`R+3` yes-vote, because the voter's unanimous parents would
//! intersect that unit's parents in an honest creator above the
//! candidate — so it is globally doomed to decide 0. Every round-R
//! unit we have never even heard of is outside our held `R+3` unit's
//! ancestry, so no unknown candidate can outrank a chosen head.
//!
//! On commit, the head's not-yet-emitted causal ancestors are
//! extracted BFS-style and sent through the ordered-item channel in
//! oldest-first order. An equivocator's sibling branches may both be
//! swept as ancestry — the guarantee is one identical order on every
//! node, not single-branch emission; item processing downstream
//! validates each item on its own terms.

use std::collections::{BTreeMap, VecDeque};

use bitcoin::hashes::Hash as _;
use picomint_encoding::Encodable;
use picomint_redb::{DbRead, Table};

use crate::data::DataProvider;
use crate::engine::Engine;
use crate::unit::{Round, Unit, UnitData, UnitEnvelope, UnitHash};

/// The common-vote bit for a candidate of round `candidate_round` as
/// seen from round `round`: fixed 1 two rounds up (fast include),
/// fixed 0 three rounds up (fast exclusion of invisible candidates),
/// seeded pseudo-random above (tie-breaking). Identical at every node
/// by construction — commonness, not unpredictability, is what safety
/// needs under a benign network. The round input is load-bearing: the
/// same candidate is re-evaluated at successive rounds and needs a
/// fresh bit each time, or a middle-band candidate could never
/// resolve.
fn common_vote(candidate: UnitHash, candidate_round: Round, round: Round) -> bool {
    match round - candidate_round {
        2 => true,
        3 => false,
        _ => (round, candidate).consensus_hash_sha256().to_byte_array()[0] & 1 == 1,
    }
}

/// The vote of extended unit `unit` on `candidate`: direct parent
/// membership one round up, unanimity-else-common-vote above, memoized
/// in `votes` (votes are pure functions of the unit's fixed ancestry).
///
/// Free over the two engine fields it touches so `decide` can walk
/// borrowed parent maps of `extended` while the tally mutates `votes`.
fn vote(
    extended: &BTreeMap<UnitHash, Unit>,
    votes: &mut BTreeMap<(UnitHash, UnitHash), bool>,
    candidate: UnitHash,
    unit: UnitHash,
) -> bool {
    let cand = extended.get(&candidate).expect("candidate is extended");

    let voter = extended.get(&unit).expect("voter is extended");

    if voter.round == cand.round + 1 {
        return voter.parents.get(&cand.creator) == Some(&candidate);
    }

    if let Some(bit) = votes.get(&(candidate, unit)) {
        return *bit;
    }

    let default = common_vote(candidate, cand.round, voter.round);

    let parent_votes: Vec<bool> = voter
        .parents
        .values()
        .map(|parent| vote(extended, votes, candidate, *parent))
        .collect();

    let bit = if parent_votes.iter().all(|vote| *vote) {
        true
    } else if parent_votes.iter().all(|vote| !*vote) {
        false
    } else {
        default
    };

    votes.insert((candidate, unit), bit);

    bit
}

impl<P, D, T> Engine<P, D, T>
where
    D: UnitData,
    P: DataProvider<D>,
    T: Table<Key = UnitHash, Value = UnitEnvelope<D>>,
{
    /// Drain round heads from `self.next_decide_round` upward while
    /// each round resolves. For every head, BFS-extract the
    /// not-yet-emitted causal ancestors (oldest-first) and send each
    /// item through `self.ordered_tx`.
    pub(crate) async fn run_extender(&mut self, dbtx: &impl DbRead) {
        while let Some(head) = self.choose_head(self.next_decide_round) {
            let batch = self.bfs_batch(dbtx, head);

            for ev in batch {
                for item in ev.data {
                    // Unbounded channel; send() returns Err only
                    // when the receiver is dropped — which means
                    // the daemon is gone and we'd be shutting
                    // down anyway.
                    let _ = self
                        .ordered_tx
                        .send((ev.unit.round, ev.unit.creator, item))
                        .await;
                }

                if ev.unit.creator == self.id {
                    self.unordered_own_data.remove(&ev.unit.round);
                }
            }

            self.next_decide_round += 1;
        }
    }

    /// Resolve `round`'s head: walk the extended round candidates in
    /// ascending hash order. The first candidate decided 1 is the head; an
    /// undecided candidate means wait for more rounds. Requires at
    /// least one extended `round + 3` unit — before that, an unknown
    /// candidate could still be decided 1 elsewhere and outrank any
    /// head chosen from the local candidate set.
    ///
    /// A full walk with every candidate decided 0 also waits — the
    /// paper's trailing `output ⊥`, dead under its Lemma C.13 (some
    /// candidate is referenced by `f+1` honest next-round units, so
    /// every unit two rounds up votes 1 on it and it can never gather
    /// a 0-certificate) whenever every honest node eventually holds a
    /// unit in the next round, which submission fan-out and sequential
    /// own-round backfill provide.
    fn choose_head(&mut self, round: Round) -> Option<UnitHash> {
        if self.extended_at(round + 3).is_empty() {
            return None;
        }

        for candidate in self.extended_at(round) {
            match self.decide(candidate) {
                Some(true) => return Some(candidate),
                Some(false) => continue,
                None => return None,
            }
        }

        None
    }

    /// The candidate's include/exclude decision, or `None` while
    /// undecided: scan extended units two or more rounds above the
    /// candidate for one whose parents carry `2f+1` votes matching the
    /// round's common vote. Decisions are stable — they propagate to
    /// every later round's votes — so they are cached for the engine's
    /// lifetime.
    fn decide(&mut self, candidate: UnitHash) -> Option<bool> {
        if let Some(bit) = self.decided.get(&candidate) {
            return Some(*bit);
        }

        let candidate_round = self
            .extended
            .get(&candidate)
            .expect("candidates are extended")
            .round;

        for round in (candidate_round + 2).. {
            if self.extended_at(round).is_empty() {
                return None;
            }

            let v = common_vote(candidate, candidate_round, round);

            for unit in self.extended_at(round) {
                let parents = &self
                    .extended
                    .get(&unit)
                    .expect("iterated over extended units")
                    .parents;

                let matching = parents
                    .values()
                    .filter(|parent| {
                        vote(&self.extended, &mut self.votes, candidate, **parent) == v
                    })
                    .count();

                if matching >= self.n.threshold() {
                    self.decided.insert(candidate, v);

                    return Some(v);
                }
            }
        }

        None
    }

    /// BFS over the head's not-yet-emitted ancestors, marking each
    /// visited unit in `self.emitted` as we enqueue it. Returns the
    /// envelopes oldest-first (reversed BFS): rounds ascend since
    /// parents sit exactly one round down, so every node's own units
    /// emit in submission order. Within a round the order is BFS
    /// discovery — a deterministic function of the head and the
    /// emitted set, hence identical on every node, which is all the
    /// ordering needs; the paper's hash tie-break within rounds is
    /// not load-bearing.
    fn bfs_batch(&mut self, dbtx: &impl DbRead, head: UnitHash) -> Vec<UnitEnvelope<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        assert!(self.emitted.insert(head));

        let ev = dbtx
            .get(&self.units_table, &head)
            .expect("commit head is stored");

        queue.push_back(ev);

        while let Some(ev) = queue.pop_front() {
            for parent in ev.unit.parents.values() {
                if self.emitted.contains(parent) {
                    continue;
                }

                let p = dbtx
                    .get(&self.units_table, parent)
                    .expect("ancestors of an extended head are stored");

                // Tentatively mark so the deeper BFS doesn't enqueue twice.
                self.emitted.insert(*parent);

                queue.push_back(p);
            }

            batch.push(ev);
        }

        batch.reverse();

        batch
    }
}
