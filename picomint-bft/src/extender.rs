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
//! extracted BFS-style and emitted as the round's batch.

use std::collections::{BTreeMap, VecDeque};

use async_channel::Sender;
use bitcoin::hashes::sha256;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::Encodable;

use crate::unit::{Round, Unit, UnitData};

/// Drives leader-vote ordering over a stream of confirmed units.
///
/// Units are fed one at a time via [`Self::add_unit`]; whenever the
/// new unit unlocks one or more leader rounds, every datum from each
/// committed batch is pushed to `ordered_tx` (oldest-first per batch).
/// The extender processes leader rounds strictly in order — round
/// `r+1`'s decision is not attempted until round `r`'s has resolved.
pub struct Extender<D: UnitData> {
    units: Units<D>,
    num_peers: NumPeers,
    next_round_to_decide: Round,
    ordered_tx: Sender<(Round, PeerId, D)>,
}

impl<D: UnitData> Extender<D> {
    /// Build an empty extender for a federation of size `n`. Ordered
    /// items emitted on commit are pushed to `ordered_tx`.
    pub fn new(n: NumPeers, ordered_tx: Sender<(Round, PeerId, D)>) -> Self {
        Self {
            units: Units::new(),
            num_peers: n,
            next_round_to_decide: 0,
            ordered_tx,
        }
    }

    /// Feed a freshly-confirmed unit. Every batch unlocked by the
    /// arrival is BFS-extracted and each contained datum is pushed to
    /// `ordered_tx` as a `(round, creator, datum)` triple, oldest-first
    /// per batch.
    pub fn add_unit(&mut self, unit: Unit<D>) {
        self.units.add(unit);

        while let Some(decision) = self.try_decide(self.next_round_to_decide) {
            if let Decision::Commit(round, creator) = decision {
                for u in self.units.remove_batch(round, creator) {
                    for d in &u.data {
                        self.ordered_tx
                            .try_send((u.round, u.creator, d.clone()))
                            .expect("ordered channel is unbounded; receiver kept alive");
                    }
                }
            }

            self.next_round_to_decide += 1;
        }
    }

    /// The deterministic random order in which to walk candidates for
    /// `round`. Seeded by the round number so every honest peer
    /// computes the same permutation.
    fn candidate_order(&self, round: Round) -> Vec<PeerId> {
        let mut peers: Vec<(PeerId, sha256::Hash)> = self
            .num_peers
            .peer_ids()
            .map(|p| (p, (round, p).consensus_hash_sha256()))
            .collect();

        peers.sort_by_key(|(_, h)| *h);

        peers.into_iter().map(|(p, _)| p).collect()
    }

    /// Try to resolve `leader_round`'s verdict by walking candidates
    /// in the round's random order. Returns `None` when the current
    /// candidate is still undecided — caller re-tries after more units
    /// confirm.
    fn try_decide(&self, leader_round: Round) -> Option<Decision> {
        for candidate_peer in self.candidate_order(leader_round) {
            match self.decide_candidate(leader_round, candidate_peer) {
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
        leader_round: Round,
        candidate_peer: PeerId,
    ) -> Option<CandidateOutcome> {
        // Vote table built incrementally over rounds R+1, R+2, ….
        // Keyed by (round, creator) since at most one confirmed unit
        // per slot exists — slot uniquely identifies the voter.
        let mut votes: BTreeMap<(Round, PeerId), bool> = BTreeMap::new();

        for k in (leader_round..self.units.highest_round()).map(|k| k + 1) {
            let mut yes = 0;
            let mut no = 0;

            for unit in self.units.in_round(k) {
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

                    assert_eq!(yes_parents + no_parents, self.num_peers.threshold());

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

            if yes >= self.num_peers.threshold() {
                return Some(CandidateOutcome::Yes);
            }

            if no >= self.num_peers.threshold() {
                return Some(CandidateOutcome::No);
            }
        }

        None
    }
}

enum CandidateOutcome {
    Yes,
    No,
}

enum Decision {
    Commit(Round, PeerId),
    Skip,
}

/// In-memory store of confirmed units, keyed by `(round, creator)`.
/// Slot-keying is enough because consistent broadcast guarantees at most
/// one confirmed unit per slot, and `BTreeMap`'s `range` gives us
/// round-restricted iteration without a separate by-round index.
struct Units<D: UnitData> {
    by_slot: BTreeMap<(Round, PeerId), Unit<D>>,
    highest_round: Round,
}

impl<D: UnitData> Units<D> {
    fn new() -> Self {
        Self {
            by_slot: BTreeMap::new(),
            highest_round: 0,
        }
    }

    fn add(&mut self, unit: Unit<D>) {
        self.highest_round = self.highest_round.max(unit.round);

        self.by_slot.insert((unit.round, unit.creator), unit);
    }

    fn in_round(&self, round: Round) -> impl Iterator<Item = &Unit<D>> {
        self.by_slot
            .range((round, PeerId::from(0u8))..)
            .take_while(move |((r, _), _)| *r == round)
            .map(|(_, u)| u)
    }

    fn highest_round(&self) -> Round {
        self.highest_round
    }

    /// BFS over the `(round, creator)` head's ancestors, removing each
    /// from the store as we visit it. Already-emitted ancestors (removed
    /// by an earlier batch) are skipped — that's how successive batches
    /// partition the DAG. Returns oldest-first.
    fn remove_batch(&mut self, round: Round, creator: PeerId) -> Vec<Unit<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        let head = self.by_slot.remove(&(round, creator)).expect("head exists");

        queue.push_back(head);

        while let Some(unit) = queue.pop_front() {
            for parent_creator in &unit.parents {
                let parent_round = unit.round - 1;

                if let Some(parent) = self.by_slot.remove(&(parent_round, *parent_creator)) {
                    queue.push_back(parent);
                }
            }

            batch.push(unit);
        }

        batch.reverse();

        batch
    }
}
