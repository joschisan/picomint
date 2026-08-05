//! Per-peer order extender running a deterministic QuickAleph-style
//! virtual-voting rule (arXiv:1908.05156, Appendix A/C.2) with the
//! coin replaced by seeded pseudo-random bits — sound under the
//! benign-network model, where common votes only need to be *common*,
//! not unpredictable.
//!
//! For each round R, every extended round-R branch is a *candidate*,
//! walked in a deterministic priority order seeded by the round. Each
//! candidate resolves to a binary include/exclude decision:
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
//! The round head is the first candidate in priority order decided 1,
//! after every earlier candidate is decided 0. Walking a deterministic
//! (rather than secret) priority order is safe because of the coverage
//! lemma (C.10): a round-R candidate outside the ancestry of *any*
//! round-`R+3` unit can never gather a round-`R+3` yes-vote — the
//! voter's unanimous parents would intersect that unit's parents in an
//! honest creator above the candidate — so it is globally doomed to
//! decide 0 and may be skipped without waiting. In particular every
//! round-R unit we have never even heard of is outside our held
//! `R+3` ancestry, so no unknown candidate can outrank a chosen head.
//!
//! On commit, the head's not-yet-emitted causal ancestors are
//! extracted BFS-style and sent through the ordered-item channel in
//! oldest-first order. An equivocator's sibling branches may both be
//! swept as ancestry — the guarantee is one identical order on every
//! peer, not single-branch emission; item processing downstream
//! validates each item on its own terms.

use std::collections::{BTreeSet, VecDeque};

use bitcoin::hashes::Hash as _;
use bitcoin::hashes::sha256;
use picomint_core::PeerId;
use picomint_encoding::Encodable;
use picomint_redb::DbRead;

use crate::data::DataProvider;
use crate::engine::Engine;
use crate::unit::{Round, Slot, Unit, UnitData, UnitHash, round_range};

enum Decision {
    Commit(Slot),
    Skip,
}

/// The common-vote bit for a candidate at round `candidate.0` as seen
/// from round `round`: fixed 1 two rounds up (fast include), fixed 0
/// three rounds up (fast exclusion of invisible candidates), seeded
/// pseudo-random above (tie-breaking). Identical at every peer by
/// construction — commonness, not unpredictability, is what safety
/// needs under a benign network.
fn common_vote(candidate: Slot, round: Round) -> bool {
    match round - candidate.0 {
        2 => true,
        3 => false,
        _ => (round, candidate.2).consensus_hash_sha256().to_byte_array()[0] & 1 == 1,
    }
}

/// The deterministic priority by which round-`round` candidates are
/// walked; seeded by the round number so every peer computes the same
/// order.
fn priority(round: Round, candidate: Slot) -> sha256::Hash {
    (round, candidate.2).consensus_hash_sha256()
}

impl<P, D> Engine<P, D>
where
    D: UnitData,
    P: DataProvider<D>,
{
    /// Drain round decisions from `self.next_decide_round` upward
    /// while each one resolves to `Commit` or `Skip`. For every
    /// committed head, BFS-extract the not-yet-emitted causal
    /// ancestors (oldest-first) and send each item through
    /// `self.ordered_tx`.
    pub(crate) async fn run_extender(&mut self, dbtx: &impl DbRead) {
        while let Some(decision) = self.choose_head(self.next_decide_round) {
            if let Decision::Commit(head) = decision {
                let batch = self.bfs_batch(dbtx, head);

                for u in batch {
                    for item in u.data {
                        // Unbounded channel; send() returns Err only
                        // when the receiver is dropped — which means
                        // the daemon is gone and we'd be shutting
                        // down anyway.
                        let _ = self.ordered_tx.send((u.round, u.creator, item)).await;
                    }

                    if u.creator == self.id {
                        self.unordered_own_data.remove(&u.round);
                    }
                }
            }

            self.next_decide_round += 1;

            // Decisions only ever concern candidates at or above the
            // cursor, so cached verdicts and votes below it are dead
            // weight.
            let bound = (self.next_decide_round, PeerId::from(0u8), UnitHash::min());

            self.decided = self.decided.split_off(&bound);

            self.votes = self
                .votes
                .split_off(&(bound, (0, PeerId::from(0u8), UnitHash::min())));
        }
    }

    fn highest_extended_round(&self) -> Round {
        self.extended
            .iter()
            .next_back()
            .map_or(0, |((r, _, _), _)| *r)
    }

    /// Resolve `round`'s head: walk the extended round candidates in
    /// priority order, skipping candidates decided 0 and candidates
    /// outside our `round + 3` coverage (globally doomed to 0 by the
    /// coverage lemma). The first candidate decided 1 is the head; an
    /// undecided candidate means wait for more rounds. Requires at
    /// least one extended `round + 3` unit — before that, no coverage
    /// statement about unknown candidates is possible.
    fn choose_head(&mut self, round: Round) -> Option<Decision> {
        let cover = *self.extended.range(round_range(round + 3)).next()?.0;

        let covered = self.ancestors_at(cover, round);

        let mut candidates: Vec<Slot> = self
            .extended
            .range(round_range(round))
            .map(|(slot, _)| *slot)
            .collect();

        candidates.sort_by_key(|slot| priority(round, *slot));

        for candidate in candidates {
            if !covered.contains(&candidate) {
                continue;
            }

            match self.decide(candidate) {
                Some(true) => return Some(Decision::Commit(candidate)),
                Some(false) => continue,
                None => return None,
            }
        }

        Some(Decision::Skip)
    }

    /// The round-`round` slots in the causal history of `top`,
    /// collected by a level walk over the in-memory parent maps.
    fn ancestors_at(&self, top: Slot, round: Round) -> BTreeSet<Slot> {
        let mut level: BTreeSet<Slot> = BTreeSet::from([top]);

        for _ in round..top.0 {
            level = level
                .iter()
                .flat_map(|slot| {
                    let parents = self
                        .extended
                        .get(slot)
                        .expect("extended unit's ancestry is extended");

                    parents
                        .iter()
                        .map(move |(creator, hash)| (slot.0 - 1, *creator, *hash))
                })
                .collect();
        }

        level
    }

    /// The candidate's include/exclude decision, or `None` while
    /// undecided: scan extended units two or more rounds above the
    /// candidate for one whose parents carry `2f+1` votes matching the
    /// round's common vote. Decisions are stable — they propagate to
    /// every later round's votes — so they are cached for the engine's
    /// lifetime and pruned below the cursor.
    fn decide(&mut self, candidate: Slot) -> Option<bool> {
        if let Some(bit) = self.decided.get(&candidate) {
            return Some(*bit);
        }

        for round in (candidate.0 + 2)..=self.highest_extended_round() {
            let v = common_vote(candidate, round);

            let deciders: Vec<Slot> = self
                .extended
                .range(round_range(round))
                .map(|(slot, _)| *slot)
                .collect();

            for unit in deciders {
                let parents = self
                    .extended
                    .get(&unit)
                    .expect("iterated over extended units")
                    .clone();

                let matching = parents
                    .iter()
                    .filter(|(creator, hash)| {
                        self.vote(candidate, (unit.0 - 1, **creator, **hash)) == v
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

    /// The vote of extended unit `unit` on `candidate`: direct parent
    /// membership one round up, unanimity-else-common-vote above,
    /// memoized in `self.votes` (votes are pure functions of the
    /// unit's fixed ancestry).
    fn vote(&mut self, candidate: Slot, unit: Slot) -> bool {
        if unit.0 == candidate.0 + 1 {
            return self
                .extended
                .get(&unit)
                .expect("voter is extended")
                .get(&candidate.1)
                == Some(&candidate.2);
        }

        if let Some(bit) = self.votes.get(&(candidate, unit)) {
            return *bit;
        }

        let parents = self.extended.get(&unit).expect("voter is extended").clone();

        let parent_votes: Vec<bool> = parents
            .iter()
            .map(|(creator, hash)| self.vote(candidate, (unit.0 - 1, *creator, *hash)))
            .collect();

        let bit = if parent_votes.iter().all(|vote| *vote) {
            true
        } else if parent_votes.iter().all(|vote| !*vote) {
            false
        } else {
            common_vote(candidate, unit.0)
        };

        self.votes.insert((candidate, unit), bit);

        bit
    }

    /// BFS over the head's not-yet-emitted ancestors, marking each
    /// visited slot in `self.emitted` as we enqueue it. Returns the
    /// units oldest-first so the caller emits them in that order.
    fn bfs_batch(&mut self, dbtx: &impl DbRead, head: Slot) -> Vec<Unit<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        if !self.emitted.insert(head) {
            return batch;
        }

        let signed = dbtx
            .get(&self.units_table, &head)
            .expect("commit head is stored");

        queue.push_back(signed.unit);

        while let Some(unit) = queue.pop_front() {
            if let Some(parent_round) = unit.round.checked_sub(1) {
                for (parent_creator, parent_hash) in &unit.parents {
                    let slot = (parent_round, *parent_creator, *parent_hash);

                    if self.emitted.contains(&slot) {
                        continue;
                    }

                    if let Some(p) = dbtx.get(&self.units_table, &slot) {
                        // Tentatively mark so the deeper BFS doesn't enqueue twice.
                        self.emitted.insert(slot);

                        queue.push_back(p.unit);
                    }
                }
            }

            batch.push(unit);
        }

        batch.reverse();

        batch
    }
}
