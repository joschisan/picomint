//! Per-peer order extender driven by leader-vote consensus, with a
//! random per-round candidate ordering.
//!
//! For each round R, every peer is a *candidate* head. The candidates
//! are walked in a deterministic random order seeded by the round
//! number. For each candidate `c`:
//!
//! - A round-`R+1` unit votes **yes** iff `c`'s hash appears in its
//!   parent set, otherwise **no**.
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

use std::collections::{BTreeMap, HashMap, VecDeque};

use async_channel::Sender;
use bitcoin::hashes::sha256;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::Encodable;

use crate::unit::{Round, Unit, UnitData, UnitHash};

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
            if let Decision::Commit(head) = decision {
                for u in self.units.remove_batch(&head) {
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
            let candidate_hash = self.units.hash_at(leader_round, candidate_peer);

            match self.decide_candidate(leader_round, candidate_hash) {
                Some(CandidateOutcome::Yes(head)) => return Some(Decision::Commit(head)),
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
        candidate_hash: Option<UnitHash>,
    ) -> Option<CandidateOutcome> {
        // Vote table built incrementally over rounds R+1, R+2, ….
        let mut votes: HashMap<UnitHash, bool> = HashMap::new();

        for k in (leader_round..self.units.highest_round()).map(|k| k + 1) {
            let mut yes = 0;
            let mut no = 0;

            for unit in self.units.in_round(k) {
                let vote = if k == leader_round + 1 {
                    candidate_hash.is_some_and(|lh| unit.parents.values().any(|h| *h == lh))
                } else {
                    let mut yes_parents = 0;
                    let mut no_parents = 0;

                    for ph in unit.parents.values() {
                        if *votes.get(ph).expect("computed in the previous round") {
                            yes_parents += 1;
                        } else {
                            no_parents += 1;
                        }
                    }

                    assert_eq!(yes_parents + no_parents, self.num_peers.threshold());

                    assert_ne!(yes_parents, no_parents);

                    yes_parents > no_parents
                };

                votes.insert(unit.hash(), vote);

                if vote {
                    yes += 1;
                } else {
                    no += 1;
                }
            }

            if yes >= self.num_peers.threshold() {
                return Some(CandidateOutcome::Yes(candidate_hash.expect(
                    "yes-quorum at R+1 requires candidate's hash in voters' parents",
                )));
            }

            if no >= self.num_peers.threshold() {
                return Some(CandidateOutcome::No);
            }
        }

        None
    }
}

enum CandidateOutcome {
    Yes(UnitHash),
    No,
}

enum Decision {
    Commit(UnitHash),
    Skip,
}

/// In-memory store of confirmed units, indexed for O(1) slot lookup
/// and round iteration.
struct Units<D: UnitData> {
    by_hash: BTreeMap<UnitHash, Unit<D>>,
    by_round: BTreeMap<Round, Vec<UnitHash>>,
    by_slot: BTreeMap<(Round, PeerId), UnitHash>,
    highest_round: Round,
}

impl<D: UnitData> Units<D> {
    fn new() -> Self {
        Self {
            by_hash: BTreeMap::new(),
            by_round: BTreeMap::new(),
            by_slot: BTreeMap::new(),
            highest_round: 0,
        }
    }

    fn add(&mut self, unit: Unit<D>) {
        self.highest_round = self.highest_round.max(unit.round);

        self.by_round
            .entry(unit.round)
            .or_default()
            .push(unit.hash());

        self.by_slot.insert((unit.round, unit.creator), unit.hash());

        self.by_hash.insert(unit.hash(), unit);
    }

    fn hash_at(&self, round: Round, creator: PeerId) -> Option<UnitHash> {
        self.by_slot.get(&(round, creator)).copied()
    }

    fn in_round(&self, round: Round) -> impl Iterator<Item = &Unit<D>> {
        self.by_round
            .get(&round)
            .into_iter()
            .flatten()
            .filter_map(|h| self.by_hash.get(h))
    }

    fn highest_round(&self) -> Round {
        self.highest_round
    }

    /// BFS over `head`'s ancestors, removing each from the store as we
    /// visit it. Already-emitted ancestors (removed by an earlier
    /// batch) are skipped — that's how successive batches partition the
    /// DAG. Returns oldest-first.
    fn remove_batch(&mut self, head: &UnitHash) -> Vec<Unit<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        let head_unit = self.by_hash.remove(head).expect("head exists");

        self.by_slot.remove(&(head_unit.round, head_unit.creator));

        queue.push_back(head_unit);

        while let Some(unit) = queue.pop_front() {
            for parent_hash in unit.parents.values() {
                if let Some(parent) = self.by_hash.remove(parent_hash) {
                    self.by_slot.remove(&(parent.round, parent.creator));

                    queue.push_back(parent);
                }
            }

            batch.push(unit);
        }

        batch.reverse();

        batch
    }
}
