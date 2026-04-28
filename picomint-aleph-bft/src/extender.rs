use std::collections::{BTreeMap, HashMap, VecDeque};

use picomint_core::PeerId;

use crate::unit::{Round, Unit, UnitData, UnitHash};

/// Drives Aleph-style head election and BFS batch extraction over a stream
/// of confirmed units.
///
/// Units are fed one at a time via `add_unit`; the unit's parents must
/// already have been fed previously (this matches the natural ordering of
/// confirmation events in the engine, since a unit only confirms after all
/// its parents are confirmed). For each round in sequence the extender
/// elects a head and emits the BFS-extracted batch of head's not-yet-emitted
/// ancestors. Rounds emit zero or more batches per `add_unit` call,
/// depending on how many newly-electable rounds the new unit unlocks.
pub struct Extender<D: UnitData> {
    units: Units<D>,
    election: Option<RoundElection>,
    next_round: Round,
    threshold: usize,
}

impl<D: UnitData> Extender<D> {
    /// Build an empty extender for a federation with the given confirmation
    /// threshold.
    pub fn new(threshold: usize) -> Self {
        Self {
            units: Units::new(),
            election: None,
            next_round: 0,
            threshold,
        }
    }

    /// Feed a freshly-confirmed unit to the extender. Returns any batches
    /// of ordered units that this unit unlocks; each batch is the BFS
    /// extraction (oldest-first) of one round-head's not-yet-emitted
    /// ancestors.
    pub fn add_unit(&mut self, unit: Unit<D>) -> Vec<Vec<Unit<D>>> {
        self.units.add(unit.clone());

        let mut batches = Vec::new();

        if let Some(election) = self.election.take() {
            let result = election.add_voter(&unit, &self.units, self.threshold);

            if let Some(batch) = self.handle_election(result) {
                batches.push(batch);
            }
        }

        while self.election.is_none() {
            match RoundElection::for_round(self.next_round, &self.units, self.threshold) {
                Ok(result) => {
                    if let Some(batch) = self.handle_election(result) {
                        batches.push(batch);
                    }
                }
                Err(()) => break,
            }
        }

        batches
    }

    fn handle_election(&mut self, result: ElectionResult) -> Option<Vec<Unit<D>>> {
        match result {
            ElectionResult::Pending(election) => {
                self.election = Some(election);
                None
            }
            ElectionResult::Elected(head) => {
                self.next_round += 1;
                Some(self.units.remove_batch(&head))
            }
        }
    }
}

/// In-memory store of confirmed units indexed for fast batch extraction.
struct Units<D: UnitData> {
    by_hash: BTreeMap<UnitHash, Unit<D>>,
    by_round: BTreeMap<Round, Vec<UnitHash>>,
    highest_round: Round,
}

impl<D: UnitData> Units<D> {
    fn new() -> Self {
        Self {
            by_hash: BTreeMap::new(),
            by_round: BTreeMap::new(),
            highest_round: 0,
        }
    }

    fn add(&mut self, unit: Unit<D>) {
        if unit.round > self.highest_round {
            self.highest_round = unit.round;
        }

        self.by_round
            .entry(unit.round)
            .or_default()
            .push(unit.hash());

        self.by_hash.insert(unit.hash(), unit);
    }

    fn get(&self, hash: &UnitHash) -> Option<&Unit<D>> {
        self.by_hash.get(hash)
    }

    fn in_round(&self, round: Round) -> Option<&[UnitHash]> {
        self.by_round.get(&round).map(Vec::as_slice)
    }

    fn highest_round(&self) -> Round {
        self.highest_round
    }

    /// BFS over `head`'s ancestors, removing each from the store as it's
    /// visited. Returned units are oldest-first (round-0 ancestors at the
    /// front, head at the back). Already-emitted ancestors (removed by an
    /// earlier batch) are skipped — that's how successive batches partition
    /// the DAG.
    fn remove_batch(&mut self, head: &UnitHash) -> Vec<Unit<D>> {
        let mut batch = Vec::new();
        let mut queue = VecDeque::new();

        queue.push_back(self.by_hash.remove(head).expect("head exists"));

        while let Some(unit) = queue.pop_front() {
            for parent_hash in unit.parents.values() {
                if let Some(parent) = self.by_hash.remove(parent_hash) {
                    queue.push_back(parent);
                }
            }

            batch.push(unit);
        }

        batch.reverse();

        batch
    }
}

/// Aleph's deterministic common-vote schedule: at relative round 2 it's
/// `true`, at 3 `false`, at 4 `true`, then alternates `true` on odd / `false`
/// on even from 5 onward. Used as the fallback vote when a voter's parents
/// are split, and as the value that — when reached by a threshold of
/// parents at relative round ≥ 3 — locks in the candidate's election outcome.
fn common_vote(relative_round: Round) -> bool {
    if relative_round == 3 {
        return false;
    }

    if relative_round <= 4 {
        return true;
    }

    relative_round % 2 == 1
}

enum CandidateOutcome {
    Eliminate,
    ElectionDone(UnitHash),
}

/// Running election for a single candidate at `(candidate_round,
/// candidate_creator)`. Walks voters round-by-round, caching each voter's
/// vote; either reaches a decision (Elected / Eliminated) or remains
/// Pending pending more voters.
struct CandidateElection {
    candidate_round: Round,
    candidate_creator: PeerId,
    candidate_hash: UnitHash,
    votes: HashMap<UnitHash, bool>,
}

impl CandidateElection {
    fn for_candidate<D: UnitData>(
        candidate: &Unit<D>,
        units: &Units<D>,
        threshold: usize,
    ) -> Result<Self, CandidateOutcome> {
        let mut election = CandidateElection {
            candidate_round: candidate.round,
            candidate_creator: candidate.creator,
            candidate_hash: candidate.hash(),
            votes: HashMap::new(),
        };

        for round in election.candidate_round + 1..=units.highest_round() {
            for hash in units.in_round(round).expect("units come in order") {
                let voter = units.get(hash).expect("hash from same store");
                election.vote(voter, threshold)?;
            }
        }

        Ok(election)
    }

    fn vote<D: UnitData>(
        &mut self,
        voter: &Unit<D>,
        threshold: usize,
    ) -> Result<(), CandidateOutcome> {
        let voter_hash = voter.hash();

        if self.votes.contains_key(&voter_hash) {
            return Ok(());
        }

        if voter.round <= self.candidate_round {
            return Ok(());
        }

        let relative_round = voter.round - self.candidate_round;

        let vote = match relative_round {
            0 => unreachable!("just checked voter.round > candidate_round"),
            1 => voter.parents.get(&self.candidate_creator) == Some(&self.candidate_hash),
            _ => self.vote_from_parents(voter, threshold, relative_round)?,
        };

        self.votes.insert(voter_hash, vote);

        Ok(())
    }

    fn vote_from_parents<D: UnitData>(
        &self,
        voter: &Unit<D>,
        threshold: usize,
        relative_round: Round,
    ) -> Result<bool, CandidateOutcome> {
        let mut votes_for: usize = 0;
        let mut votes_against: usize = 0;

        for parent_hash in voter.parents.values() {
            match *self.votes.get(parent_hash).expect("units come in order") {
                true => votes_for += 1,
                false => votes_against += 1,
            }
        }

        let cv = common_vote(relative_round);

        if relative_round >= 3 {
            match cv {
                true if votes_for >= threshold => {
                    return Err(CandidateOutcome::ElectionDone(self.candidate_hash));
                }
                false if votes_against >= threshold => {
                    return Err(CandidateOutcome::Eliminate);
                }
                _ => {}
            }
        }

        Ok(match (votes_for, votes_against) {
            (0, _) => false,
            (_, 0) => true,
            _ => cv,
        })
    }

    fn add_voter<D: UnitData>(
        mut self,
        voter: &Unit<D>,
        threshold: usize,
    ) -> Result<Self, CandidateOutcome> {
        self.vote(voter, threshold)?;
        Ok(self)
    }
}

enum ElectionResult {
    Pending(RoundElection),
    Elected(UnitHash),
}

/// Election for a single round's head. Walks the round's candidates in
/// hash order; on Eliminate falls through to the next candidate, on
/// ElectionDone yields the winner. Wraps a `CandidateElection` for the
/// currently-active candidate.
struct RoundElection {
    /// Remaining candidates, hash-sorted then reversed; we `pop` from the
    /// back so the lowest-hash candidate is tried first.
    candidates: Vec<UnitHash>,
    voting: CandidateElection,
}

impl RoundElection {
    fn for_round<D: UnitData>(
        round: Round,
        units: &Units<D>,
        threshold: usize,
    ) -> Result<ElectionResult, ()> {
        if units.highest_round() < round + 3 {
            return Err(());
        }

        let mut candidates: Vec<UnitHash> =
            units.in_round(round).expect("units come in order").to_vec();

        candidates.sort();
        candidates.reverse();

        let first = candidates.pop().expect("at least one candidate at round");
        let candidate = units.get(&first).expect("hash from same store");

        Ok(Self::handle_result(
            CandidateElection::for_candidate(candidate, units, threshold),
            candidates,
            units,
            threshold,
        ))
    }

    fn handle_result<D: UnitData>(
        result: Result<CandidateElection, CandidateOutcome>,
        mut candidates: Vec<UnitHash>,
        units: &Units<D>,
        threshold: usize,
    ) -> ElectionResult {
        match result {
            Ok(voting) => ElectionResult::Pending(RoundElection { candidates, voting }),
            Err(CandidateOutcome::ElectionDone(head)) => ElectionResult::Elected(head),
            Err(CandidateOutcome::Eliminate) => {
                let next = candidates
                    .pop()
                    .expect("eliminate guarantees more candidates");

                let candidate = units.get(&next).expect("hash from same store");

                Self::handle_result(
                    CandidateElection::for_candidate(candidate, units, threshold),
                    candidates,
                    units,
                    threshold,
                )
            }
        }
    }

    fn add_voter<D: UnitData>(
        self,
        voter: &Unit<D>,
        units: &Units<D>,
        threshold: usize,
    ) -> ElectionResult {
        Self::handle_result(
            self.voting.add_voter(voter, threshold),
            self.candidates,
            units,
            threshold,
        )
    }
}
