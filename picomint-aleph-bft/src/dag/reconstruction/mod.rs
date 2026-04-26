use crate::{
    units::{ControlHash, FullUnit, Unit, UnitCoord, UnitWithParents, WrappedUnit},
    NodeMap, SessionId, UnitHash,
};
use aleph_bft_rmc::NumPeers;
use std::collections::HashMap;

mod dag;
mod parents;

use aleph_bft_types::{Data, OrderedUnit, PeerId, Round, Signed};
use dag::Dag;
use parents::Reconstruction as ParentReconstruction;

/// A unit with its parents represented explicitly.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ReconstructedUnit<U: Unit> {
    unit: U,
    parents: NodeMap<(UnitHash, Round)>,
}

impl<U: Unit> ReconstructedUnit<U> {
    /// Returns a reconstructed unit if the parents agree with the hash, errors out otherwise.
    pub fn with_parents(unit: U, parents: NodeMap<(UnitHash, Round)>) -> Result<Self, U> {
        match unit.control_hash().combined_hash() == ControlHash::create_control_hash(&parents) {
            true => Ok(ReconstructedUnit { unit, parents }),
            false => Err(unit),
        }
    }

    /// Reconstructs empty parents for a round 0 unit.
    /// Assumes obviously incorrect units with wrong control hashes have been rejected earlier.
    /// Will panic if called for any other kind of unit.
    pub fn initial(unit: U) -> Self {
        let n_members = unit.control_hash().n_members();
        assert!(unit.round() == 0, "Only the zeroth unit can be initial.");
        ReconstructedUnit {
            unit,
            parents: NodeMap::with_size(n_members),
        }
    }
}

impl<U: Unit> Unit for ReconstructedUnit<U> {
    fn hash(&self) -> UnitHash {
        self.unit.hash()
    }

    fn coord(&self) -> UnitCoord {
        self.unit.coord()
    }

    fn control_hash(&self) -> &ControlHash {
        self.unit.control_hash()
    }

    fn session_id(&self) -> SessionId {
        self.unit.session_id()
    }
}

impl<U: Unit> WrappedUnit for ReconstructedUnit<U> {
    type Wrapped = U;

    fn unpack(self) -> U {
        self.unit
    }
}

impl<U: Unit> UnitWithParents for ReconstructedUnit<U> {
    fn parents(&self) -> impl Iterator<Item = &UnitHash> {
        self.parents.values().map(|(hash, _)| hash)
    }

    fn direct_parents(&self) -> impl Iterator<Item = &UnitHash> {
        self.parents
            .values()
            .filter_map(|(hash, parent_round)| match self.unit.coord().round() {
                // round 0 units cannot have non-empty parents
                0 => None,

                unit_round => {
                    if unit_round - 1 == *parent_round {
                        Some(hash)
                    } else {
                        None
                    }
                }
            })
    }

    fn parent_for(&self, index: PeerId) -> Option<&UnitHash> {
        self.parents.get(index).map(|(hash, _)| hash)
    }

    fn node_count(&self) -> NumPeers {
        self.parents.size()
    }
}

impl<D: Data> From<ReconstructedUnit<Signed<FullUnit<D>>>> for Option<D> {
    fn from(value: ReconstructedUnit<Signed<FullUnit<D>>>) -> Self {
        value.unpack().into_signable().into()
    }
}

impl<D: Data> From<ReconstructedUnit<Signed<FullUnit<D>>>> for OrderedUnit<D> {
    fn from(unit: ReconstructedUnit<Signed<FullUnit<D>>>) -> Self {
        let parents = unit.parents().cloned().collect();
        let unit = unit.unpack();
        let creator = unit.creator();
        let round = unit.round();
        let hash = unit.hash();
        let data = unit.into_signable().data().clone();
        OrderedUnit {
            parents,
            creator,
            round,
            hash,
            data,
        }
    }
}

/// What we need to request to reconstruct units.
#[derive(Eq, PartialEq, Debug, Clone)]
pub enum Request {
    /// We need a unit at this coordinate.
    Coord(UnitCoord),
    /// We need the explicit list of parents for the unit identified by the hash.
    /// This should only happen in the presence of forks, when optimistic reconstruction failed.
    ParentsOf(UnitHash),
}

/// The result of a reconstruction attempt. Might contain multiple reconstructed units,
/// as well as requests for some data that is needed for further reconstruction.
#[derive(Debug, PartialEq, Eq)]
pub struct ReconstructionResult<U: Unit> {
    /// All the units that got reconstructed.
    pub units: Vec<ReconstructedUnit<U>>,
    /// Any requests that now should be made.
    pub requests: Vec<Request>,
}

impl<U: Unit> ReconstructionResult<U> {
    fn new(units: Vec<ReconstructedUnit<U>>, requests: Vec<Request>) -> Self {
        ReconstructionResult { units, requests }
    }

    fn empty() -> Self {
        ReconstructionResult::new(Vec::new(), Vec::new())
    }

    fn reconstructed(unit: ReconstructedUnit<U>) -> Self {
        ReconstructionResult {
            units: vec![unit],
            requests: Vec::new(),
        }
    }

    fn request(request: Request) -> Self {
        ReconstructionResult {
            units: Vec::new(),
            requests: vec![request],
        }
    }

    fn add_unit(&mut self, unit: ReconstructedUnit<U>) {
        self.units.push(unit);
    }

    fn add_request(&mut self, request: Request) {
        self.requests.push(request);
    }

    fn accumulate(&mut self, other: ReconstructionResult<U>) {
        let ReconstructionResult {
            mut units,
            mut requests,
        } = other;
        self.units.append(&mut units);
        self.requests.append(&mut requests);
    }
}

/// The reconstruction of the structure of the Dag.
/// When passed units containing control hashes, and responses to requests it produces,
/// it eventually outputs versions with explicit parents in an order conforming to the Dag order.
pub struct Reconstruction<U: Unit> {
    parents: ParentReconstruction<U>,
    dag: Dag<ReconstructedUnit<U>>,
}

impl<U: Unit> Reconstruction<U> {
    /// Create a new reconstruction.
    pub fn new() -> Self {
        let parents = ParentReconstruction::new();
        let dag = Dag::new();
        Reconstruction { parents, dag }
    }

    fn handle_parents_reconstruction_result(
        &mut self,
        reconstruction_result: ReconstructionResult<U>,
    ) -> ReconstructionResult<U> {
        let ReconstructionResult { units, requests } = reconstruction_result;
        let units = units
            .into_iter()
            .flat_map(|unit| self.dag.add_unit(unit))
            .collect();
        ReconstructionResult::new(units, requests)
    }

    /// Add a unit to the reconstruction.
    pub fn add_unit(&mut self, unit: U) -> ReconstructionResult<U> {
        let parent_reconstruction_result = self.parents.add_unit(unit);
        self.handle_parents_reconstruction_result(parent_reconstruction_result)
    }

    /// Add an explicit list of parents to the reconstruction.
    pub fn add_parents(
        &mut self,
        unit: UnitHash,
        parents: HashMap<UnitCoord, UnitHash>,
    ) -> ReconstructionResult<U> {
        let parent_reconstruction_result = self.parents.add_parents(unit, parents);
        self.handle_parents_reconstruction_result(parent_reconstruction_result)
    }
}

#[cfg(test)]
mod test {
    use crate::{
        dag::reconstruction::{ReconstructedUnit, Reconstruction, ReconstructionResult, Request},
        units::{random_full_parent_units_up_to, Unit, UnitCoord, UnitWithParents},
        NumPeers, PeerId,
    };
    use aleph_bft_types::{NodeMap, Round};
    use rand::Rng;
    use std::collections::HashMap;

    #[test]
    fn reconstructs_initial_units() {
        let mut reconstruction = Reconstruction::new();
        for unit in &random_full_parent_units_up_to(0, NumPeers::new(4 as usize), 43)[0] {
            let ReconstructionResult {
                mut units,
                requests,
            } = reconstruction.add_unit(unit.clone());
            assert!(requests.is_empty());
            assert_eq!(units.len(), 1);
            let reconstructed_unit = units.pop().expect("just checked its there");
            assert_eq!(reconstructed_unit, ReconstructedUnit::initial(unit.clone()));
            assert_eq!(reconstructed_unit.parents().count(), 0);
        }
    }

    #[test]
    fn reconstructs_units_coming_in_order() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(7, NumPeers::new(4 as usize), 43);
        for units in &dag {
            for unit in units {
                let round = unit.round();
                let ReconstructionResult {
                    mut units,
                    requests,
                } = reconstruction.add_unit(unit.clone());
                assert!(requests.is_empty());
                assert_eq!(units.len(), 1);
                let reconstructed_unit = units.pop().expect("just checked its there");
                match round {
                    0 => {
                        assert_eq!(reconstructed_unit, ReconstructedUnit::initial(unit.clone()));
                        assert_eq!(reconstructed_unit.parents().count(), 0);
                    }
                    round => {
                        assert_eq!(reconstructed_unit.parents().count(), 4);
                        let parents = dag
                            .get((round - 1) as usize)
                            .expect("the parents are there");
                        for (parent, reconstructed_parent) in
                            parents.iter().zip(reconstructed_unit.parents())
                        {
                            assert_eq!(&parent.hash(), reconstructed_parent);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn requests_all_parents() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(1, NumPeers::new(4 as usize), 43);
        let unit = dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit");
        let ReconstructionResult { units, requests } = reconstruction.add_unit(unit.clone());
        assert!(units.is_empty());
        assert_eq!(requests.len(), 4);
    }

    #[test]
    fn requests_single_parent() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(1, NumPeers::new(4 as usize), 43);
        for unit in dag.first().expect("just created").iter().skip(1) {
            reconstruction.add_unit(unit.clone());
        }
        let unit = dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit");
        let ReconstructionResult { units, requests } = reconstruction.add_unit(unit.clone());
        assert!(units.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests.last().expect("just checked"),
            &Request::Coord(UnitCoord::new(0, PeerId::new(0 as u8)))
        );
    }

    #[test]
    fn reconstructs_units_coming_in_reverse_order() {
        let mut reconstruction = Reconstruction::new();
        let mut dag = random_full_parent_units_up_to(7, NumPeers::new(4 as usize), 43);
        dag.reverse();
        for units in dag.iter().take(7) {
            for unit in units {
                let ReconstructionResult { units, requests } =
                    reconstruction.add_unit(unit.clone());
                assert!(units.is_empty());
                assert_eq!(requests.len(), 4);
            }
        }
        for unit in dag[7].iter().take(3) {
            let ReconstructionResult { units, requests } = reconstruction.add_unit(unit.clone());
            assert!(requests.is_empty());
            assert_eq!(units.len(), 1);
        }
        let ReconstructionResult { units, requests } = reconstruction.add_unit(dag[7][3].clone());
        assert!(requests.is_empty());
        assert_eq!(units.len(), 4 * 8 - 3);
    }

    #[test]
    fn handles_bad_hash() {
        let node_count = NumPeers::new(7 as usize);
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(0, node_count, 43);
        for unit in dag.first().expect("just created") {
            reconstruction.add_unit(unit.clone());
        }
        let other_dag = random_full_parent_units_up_to(1, node_count, 43);
        let unit = other_dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit");
        let unit_hash = unit.hash();
        let ReconstructionResult { units, requests } = reconstruction.add_unit(unit.clone());
        assert!(units.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests.last().expect("just checked"),
            &Request::ParentsOf(unit_hash),
        );
        let parent_hashes: HashMap<_, _> = other_dag
            .first()
            .expect("other dag has initial units")
            .iter()
            .map(|unit| (unit.coord(), unit.hash()))
            .collect();
        let ReconstructionResult { units, requests } =
            reconstruction.add_parents(unit_hash, parent_hashes.clone());
        assert!(requests.is_empty());
        assert!(units.is_empty());
        let mut all_reconstructed = Vec::new();
        for other_initial in &other_dag[0] {
            let ReconstructionResult {
                mut units,
                requests,
            } = reconstruction.add_unit(other_initial.clone());
            assert!(requests.is_empty());
            all_reconstructed.append(&mut units);
        }
        // some of the initial units may randomly be identical,
        // so all we can say that the last reconstructed unit should be the one we want
        assert!(!all_reconstructed.is_empty());
        assert_eq!(
            all_reconstructed.pop().expect("just checked").hash(),
            unit_hash
        )
    }
    #[test]
    fn given_wrong_rounds_with_matching_hashes_when_calling_with_parents_then_err_is_returned() {
        const MAX_ROUND: Round = 7;

        let mut rng = rand::thread_rng();
        let node_count = NumPeers::new(7 as usize);
        let mut reconstruction = Reconstruction::new();

        let dag = random_full_parent_units_up_to(MAX_ROUND, node_count, 43);
        for units in &dag {
            for unit in units {
                let round = unit.round();
                let ReconstructionResult { units, requests } =
                    reconstruction.add_unit(unit.clone());
                assert!(requests.is_empty());
                assert_eq!(units.len(), 1);
                match round {
                    0 => {
                        let mut parents_map: NodeMap<(_, _)> = NodeMap::with_size(node_count);
                        assert!(
                            ReconstructedUnit::with_parents(unit.clone(), parents_map.clone())
                                .is_ok(),
                            "Initial units should not have parents!"
                        );

                        let random_parent_index = rng.gen::<u64>() % node_count.total() as u64;
                        parents_map.insert(
                            PeerId::new(random_parent_index as usize as u8),
                            (unit.hash(), 2 as Round),
                        );
                        assert_eq!(
                            ReconstructedUnit::with_parents(unit.clone(), parents_map),
                            Err(unit.clone()),
                            "Initial unit reconstructed with a non-empty parent!"
                        );
                    }
                    round => {
                        let mut parents_map: NodeMap<(_, _)> = NodeMap::with_size(node_count);
                        assert_eq!(
                            ReconstructedUnit::with_parents(unit.clone(), parents_map.clone()),
                            Err(unit.clone()),
                            "Non-initial rounds should have parents!"
                        );

                        let random_parent_index = rng.gen::<u64>() % node_count.total() as u64;
                        parents_map.insert(
                            PeerId::new(random_parent_index as usize as u8),
                            (unit.hash(), round as Round),
                        );
                        assert_eq!(
                            ReconstructedUnit::with_parents(unit.clone(), parents_map.clone()),
                            Err(unit.clone()),
                            "Unit reconstructed with missing parents and wrong parent rounds!"
                        );

                        let this_unit_control_hash = unit.control_hash();
                        let mut parents: NodeMap<(_, _)> =
                            NodeMap::with_size(this_unit_control_hash.n_members());
                        for (node_index, &(hash, round)) in units[0].parents.iter() {
                            parents.insert(node_index, (hash, round));
                        }
                        assert!(
                            ReconstructedUnit::with_parents(unit.clone(), parents.clone()).is_ok(),
                            "Reconstructed unit control hash does not match unit's control hash!"
                        );
                        let random_parent_index = rng.gen::<u64>() % node_count.total() as u64;
                        let random_parent_index = PeerId::new(random_parent_index as usize as u8);
                        let &(parent_hash, _) = parents.get(random_parent_index).unwrap();
                        let wrong_round = match round {
                            1 => MAX_ROUND,
                            _ => 0,
                        };
                        parents_map.insert(random_parent_index, (parent_hash, wrong_round));
                        assert_eq!(
                            ReconstructedUnit::with_parents(unit.clone(), parents_map.clone()),
                            Err(unit.clone()),
                            "Unit reconstructed with one parent having wrong round!"
                        );
                    }
                }
            }
        }
    }
}
