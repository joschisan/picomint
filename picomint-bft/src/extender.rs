//! Per-peer order extender driven by leader-vote consensus, with a
//! random per-round candidate ordering.
//!
//! For each round R, every peer is a *candidate* head. The candidates
//! are walked in a deterministic random order seeded by the round
//! number. For each candidate `c`:
//!
//! - A round-`R+1` unit votes **yes** iff `c` appears in its parent
//!   set, otherwise **no**.
//! - A round-`K` unit (K > R+1) votes **yes** iff the strict majority
//!   of its `2f+1` parents voted yes, otherwise **no**.
//! - If some round above R has `≥ 2f+1` yes-voters, `c` is **elected**
//!   as the head and the round commits.
//! - If `≥ 2f+1` no-voters, `c` is **eliminated**; we move to the next
//!   candidate in the order.
//! - Otherwise `c` is **undecided** — we wait for more rounds before
//!   advancing.
//!
//! If every candidate eliminates, the round is **skipped** (no head).
//! Forward induction makes elect/eliminate mutually exclusive across
//! all rounds, so all peers reach the same per-candidate verdict and
//! pick the same head.
//!
//! On commit, the head's not-yet-emitted causal ancestors are
//! extracted BFS-style and sent through the ordered-item channel in
//! oldest-first order. Each emitted slot is marked in the in-memory
//! `emitted` set so subsequent batches don't re-emit it.

use std::collections::{BTreeMap, VecDeque};

use bitcoin::hashes::sha256;
use picomint_core::PeerId;
use picomint_encoding::Encodable;
use picomint_redb::DbRead;

use crate::data::DataProvider;
use crate::engine::Engine;
use crate::unit::{Round, Unit, UnitData};

enum CandidateOutcome {
    Yes,
    No,
}

enum Decision {
    Commit(Round, PeerId),
    Skip,
}

impl<P, D> Engine<P, D>
where
    D: UnitData,
    P: DataProvider<D>,
{
    /// Drain leader-round decisions from `self.next_decide_round`
    /// upward while each one resolves to `Commit` or `Skip`. For every
    /// committed head, BFS-extract the not-yet-emitted causal ancestors
    /// (oldest-first) and send each item through `self.ordered_tx`;
    /// each emitted slot is added to `self.emitted`.
    pub(crate) async fn run_extender(&mut self, dbtx: &impl DbRead) {
        loop {
            let highest = self.highest_extended_round();

            let Some(decision) = self.try_decide(dbtx, self.next_decide_round, highest) else {
                break;
            };

            if let Decision::Commit(round, creator) = decision {
                let batch = self.bfs_batch(dbtx, round, creator);

                for u in batch {
                    for item in u.data {
                        // Unbounded channel; send() returns Err only
                        // when the receiver is dropped — which means
                        // the daemon is gone and we'd be shutting
                        // down anyway.
                        let _ = self.ordered_tx.send((u.round, u.creator, item)).await;
                    }

                    self.emitted.insert((u.round, u.creator));
                }
            }

            self.next_decide_round += 1;
        }
    }

    fn highest_extended_round(&self) -> Round {
        self.extended.iter().next_back().map_or(0, |(r, _)| *r)
    }

    /// Try to resolve `leader_round`'s verdict by walking candidates
    /// in the round's random order. Returns `None` when the current
    /// candidate is still undecided.
    fn try_decide(
        &self,
        dbtx: &impl DbRead,
        leader_round: Round,
        highest_round: Round,
    ) -> Option<Decision> {
        for candidate_peer in candidate_order(self.n, leader_round) {
            match self.decide_candidate(dbtx, leader_round, candidate_peer, highest_round) {
                Some(CandidateOutcome::Yes) => {
                    return Some(Decision::Commit(leader_round, candidate_peer));
                }
                Some(CandidateOutcome::No) => continue,
                None => return None,
            }
        }

        Some(Decision::Skip)
    }

    /// Decide a single candidate's verdict. Returns `Some(Yes)` /
    /// `Some(No)` once the corresponding 2f+1 threshold is crossed at
    /// any round above `leader_round`, or `None` if neither has been
    /// crossed yet.
    fn decide_candidate(
        &self,
        dbtx: &impl DbRead,
        leader_round: Round,
        candidate_peer: PeerId,
        highest_round: Round,
    ) -> Option<CandidateOutcome> {
        let mut votes: BTreeMap<(Round, PeerId), bool> = BTreeMap::new();

        for k in (leader_round..highest_round).map(|k| k + 1) {
            let mut yes = 0;
            let mut no = 0;

            for unit in self.round_extended_units(dbtx, k) {
                let vote = if k == leader_round + 1 {
                    unit.parents.contains(&candidate_peer)
                } else {
                    let mut yes_parents = 0;
                    let mut no_parents = 0;

                    for parent_creator in &unit.parents {
                        if *votes
                            .get(&(k - 1, *parent_creator))
                            .expect("computed in the previous round")
                        {
                            yes_parents += 1;
                        } else {
                            no_parents += 1;
                        }
                    }

                    assert_eq!(yes_parents + no_parents, self.n.threshold());

                    assert_ne!(yes_parents, no_parents);

                    yes_parents > no_parents
                };

                votes.insert((unit.round, unit.creator), vote);

                if vote {
                    yes += 1;
                } else {
                    no += 1;
                }
            }

            if yes >= self.n.threshold() {
                return Some(CandidateOutcome::Yes);
            }

            if no >= self.n.threshold() {
                return Some(CandidateOutcome::No);
            }
        }

        None
    }

    /// Extended units in `round`, used by the vote-table computation.
    /// Reads bodies on demand from `BFT_UNITS`; the slot set comes
    /// from the in-memory `extended`.
    fn round_extended_units(&self, dbtx: &impl DbRead, round: Round) -> Vec<Unit<D>> {
        self.extended
            .range((round, PeerId::from(0u8))..=(round, PeerId::from(u8::MAX)))
            .filter_map(|(r, c)| dbtx.get(&self.units_table, &(*r, *c)))
            .collect()
    }

    /// BFS over the head's not-yet-emitted ancestors, marking each
    /// visited slot in `self.emitted` as we enqueue it. Returns the
    /// units oldest-first so the caller emits them in that order.
    fn bfs_batch(&mut self, dbtx: &impl DbRead, round: Round, creator: PeerId) -> Vec<Unit<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        if self.emitted.contains(&(round, creator)) {
            return batch;
        }

        let head: Unit<D> = dbtx
            .get(&self.units_table, &(round, creator))
            .expect("commit head must exist");

        queue.push_back(head);

        while let Some(unit) = queue.pop_front() {
            if let Some(parent_round) = unit.round.checked_sub(1) {
                for parent_creator in &unit.parents {
                    if self.emitted.contains(&(parent_round, *parent_creator)) {
                        continue;
                    }
                    if let Some(p) = dbtx.get(&self.units_table, &(parent_round, *parent_creator)) {
                        // Tentatively mark so the deeper BFS doesn't enqueue twice.
                        self.emitted.insert((parent_round, *parent_creator));
                        queue.push_back(p);
                    }
                }
            }

            batch.push(unit);
        }

        batch.reverse();

        batch
    }
}

/// The deterministic random order in which to walk candidates for
/// `round`. Seeded by the round number so every honest peer
/// computes the same permutation.
fn candidate_order(n: picomint_core::NumPeers, round: Round) -> Vec<PeerId> {
    let mut peers: Vec<(PeerId, sha256::Hash)> = n
        .peer_ids()
        .map(|p| (p, (round, p).consensus_hash_sha256()))
        .collect();

    peers.sort_by_key(|(_, h)| *h);

    peers.into_iter().map(|(p, _)| p).collect()
}
