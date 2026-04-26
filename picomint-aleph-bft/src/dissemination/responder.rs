use crate::{
    collection::{NewestUnitResponse, Salt},
    dag::{DagUnit, Request},
    dissemination::DisseminationResponse,
    units::{UnitCoord, UnitStore, UnitWithParents, WrappedUnit},
    Data, Keychain, PeerId, Signed, UnitHash,
};
use std::marker::PhantomData;
use thiserror::Error;

/// A responder that is able to answer requests for data about units.
pub struct Responder<D: Data> {
    keychain: Keychain,
    _phantom: PhantomData<D>,
}

/// Ways in which it can be impossible for us to respond to a request.
#[derive(Eq, Error, Debug, PartialEq)]
pub enum Error {
    #[error("no canonical unit at {0}")]
    NoCanonicalAt(UnitCoord),
    #[error("unit with hash {0:?} not known")]
    UnknownUnit(UnitHash),
}

impl<D: Data> Responder<D> {
    /// Create a new responder.
    pub fn new(keychain: Keychain) -> Self {
        Responder {
            keychain,
            _phantom: PhantomData,
        }
    }

    fn index(&self) -> PeerId {
        self.keychain.identity()
    }

    fn on_request_coord(
        &self,
        coord: UnitCoord,
        units: &UnitStore<DagUnit<D>>,
    ) -> Result<DisseminationResponse<D>, Error> {
        units
            .canonical_unit(coord)
            .map(|unit| DisseminationResponse::Coord(unit.clone().unpack().into()))
            .ok_or(Error::NoCanonicalAt(coord))
    }

    fn on_request_parents(
        &self,
        hash: UnitHash,
        units: &UnitStore<DagUnit<D>>,
    ) -> Result<DisseminationResponse<D>, Error> {
        units
            .unit(&hash)
            .map(|unit| {
                let parents = unit
                    .parents()
                    .map(|parent_hash| {
                        units
                            .unit(parent_hash)
                            .expect("Units are added to the store in order.")
                            .clone()
                            .unpack()
                            .into_unchecked()
                    })
                    .collect();
                DisseminationResponse::Parents(hash, parents)
            })
            .ok_or(Error::UnknownUnit(hash))
    }

    fn on_request_newest(
        &self,
        requester: PeerId,
        salt: Salt,
        units: &UnitStore<DagUnit<D>>,
    ) -> DisseminationResponse<D> {
        let unit = units
            .canonical_units(requester)
            .last()
            .map(|unit| unit.clone().unpack().into_unchecked());
        let response = NewestUnitResponse::new(requester, self.index(), unit, salt);

        let signed_response = Signed::sign(response, &self.keychain).into_unchecked();
        DisseminationResponse::NewestUnit(signed_response)
    }

    /// Handle an incoming request returning either the appropriate response or an error if we
    /// aren't able to help.
    pub fn handle_request(
        &self,
        request: Request,
        units: &UnitStore<DagUnit<D>>,
    ) -> Result<DisseminationResponse<D>, Error> {
        use Request::*;
        match request {
            Coord(coord) => self.on_request_coord(coord, units),
            ParentsOf(hash) => self.on_request_parents(hash, units),
        }
    }

    /// Handle an incoming request for the newest unit of a given node we know of.
    pub fn handle_newest_unit_request(
        &self,
        requester: PeerId,
        salt: Salt,
        units: &UnitStore<DagUnit<D>>,
    ) -> DisseminationResponse<D> {
        self.on_request_newest(requester, salt, units)
    }
}

#[cfg(test)]
mod test {
    use crate::{
        dag::Request,
        dissemination::{
            responder::{Error, Responder},
            DisseminationResponse,
        },
        units::{
            random_full_parent_reconstrusted_units_up_to, TestingDagUnit, Unit, UnitCoord,
            UnitStore, UnitWithParents, WrappedUnit,
        },
        NumPeers, PeerId,
    };
    use aleph_bft_mock::{keychain, Data};
    use aleph_bft_types::Keychain;
    use std::iter::zip;

    const NODE_ID: PeerId = PeerId::new(0 as u8);
    const NODE_COUNT: NumPeers = NumPeers::new(7 as usize);

    fn keychain_set() -> Vec<Keychain> {
        (0..NODE_COUNT.total())
            .map(|i| keychain(NODE_COUNT, PeerId::new(i as u8)))
            .collect()
    }

    fn setup() -> (Responder<Data>, UnitStore<TestingDagUnit>, Vec<Keychain>) {
        let keychains = keychain_set();
        (
            Responder::new(keychains[NODE_ID.to_usize()].clone()),
            UnitStore::new(NODE_COUNT),
            keychains,
        )
    }

    #[test]
    fn empty_fails_to_respond_to_coords() {
        let (responder, store, _) = setup();
        let coord = UnitCoord::new(0, PeerId::new(1 as u8));
        let request = Request::Coord(coord);
        match responder.handle_request(request, &store) {
            Ok(response) => panic!("Unexpected response: {:?}.", response),
            Err(err) => assert_eq!(err, Error::NoCanonicalAt(coord)),
        }
    }

    #[test]
    fn empty_fails_to_respond_to_parents() {
        let (responder, store, keychains) = setup();
        let session_id = 2137;
        let hash =
            random_full_parent_reconstrusted_units_up_to(1, NODE_COUNT, session_id, &keychains)
                .last()
                .expect("just created this round")
                .last()
                .expect("the round has at least one unit")
                .hash();
        let request = Request::ParentsOf(hash);
        match responder.handle_request(request, &store) {
            Ok(response) => panic!("Unexpected response: {:?}.", response),
            Err(err) => assert_eq!(err, Error::UnknownUnit(hash)),
        }
    }

    #[test]
    fn empty_newest_responds_with_no_units() {
        let (responder, store, keychains) = setup();
        let requester = PeerId::new(1 as u8);
        let response = responder.handle_newest_unit_request(requester, rand::random(), &store);
        match response {
            DisseminationResponse::NewestUnit(newest_unit_response) => {
                let checked_newest_unit_response = newest_unit_response
                    .check(&keychains[NODE_ID.to_usize()])
                    .expect("should sign correctly");
                assert!(checked_newest_unit_response
                    .as_signable()
                    .included_data()
                    .is_empty());
            }
            other => panic!("Unexpected response: {:?}.", other),
        }
    }

    #[test]
    fn responds_to_coords_when_possible() {
        let (responder, mut store, keychains) = setup();
        let session_id = 2137;
        let coord = UnitCoord::new(3, PeerId::new(1 as u8));
        let units = random_full_parent_reconstrusted_units_up_to(
            coord.round() + 1,
            NODE_COUNT,
            session_id,
            &keychains,
        );
        for round_units in &units {
            for unit in round_units {
                store.insert(unit.clone());
            }
        }
        let request = Request::Coord(coord);
        let response = responder
            .handle_request(request, &store)
            .expect("should successfully respond");
        match response {
            DisseminationResponse::Coord(unit) => assert_eq!(
                unit,
                units[coord.round() as usize][coord.creator().to_usize()]
                    .clone()
                    .unpack()
                    .into_unchecked()
            ),
            other => panic!("Unexpected response: {:?}.", other),
        }
    }

    #[test]
    fn fails_to_responds_to_too_new_coords() {
        let (responder, mut store, keychains) = setup();
        let session_id = 2137;
        let coord = UnitCoord::new(3, PeerId::new(1 as u8));
        let units = random_full_parent_reconstrusted_units_up_to(
            coord.round() - 1,
            NODE_COUNT,
            session_id,
            &keychains,
        );
        for round_units in &units {
            for unit in round_units {
                store.insert(unit.clone());
            }
        }
        let request = Request::Coord(coord);
        match responder.handle_request(request, &store) {
            Ok(response) => panic!("Unexpected response: {:?}.", response),
            Err(err) => assert_eq!(err, Error::NoCanonicalAt(coord)),
        }
    }

    #[test]
    fn responds_to_parents_when_possible() {
        let (responder, mut store, keychains) = setup();
        let session_id = 2137;
        let units =
            random_full_parent_reconstrusted_units_up_to(5, NODE_COUNT, session_id, &keychains);
        for round_units in &units {
            for unit in round_units {
                store.insert(unit.clone());
            }
        }
        let requested_unit = units
            .last()
            .expect("just created this round")
            .last()
            .expect("the round has at least one unit")
            .clone();
        let request = Request::ParentsOf(requested_unit.hash());
        let response = responder
            .handle_request(request, &store)
            .expect("should successfully respond");
        match response {
            DisseminationResponse::Parents(response_hash, parents) => {
                assert_eq!(response_hash, requested_unit.hash());
                assert_eq!(parents.len(), requested_unit.parents().count());
                for (parent, parent_hash) in zip(parents, requested_unit.parents()) {
                    assert_eq!(&parent.as_signable().hash(), parent_hash);
                }
            }
            other => panic!("Unexpected response: {:?}.", other),
        }
    }

    #[test]
    fn fails_to_respond_to_unknown_parents() {
        let (responder, mut store, keychains) = setup();
        let session_id = 2137;
        let units =
            random_full_parent_reconstrusted_units_up_to(5, NODE_COUNT, session_id, &keychains);
        for round_units in &units {
            for unit in round_units {
                store.insert(unit.clone());
            }
        }
        let hash =
            random_full_parent_reconstrusted_units_up_to(1, NODE_COUNT, session_id, &keychains)
                .last()
                .expect("just created this round")
                .last()
                .expect("the round has at least one unit")
                .hash();
        let request = Request::ParentsOf(hash);
        match responder.handle_request(request, &store) {
            Ok(response) => panic!("Unexpected response: {:?}.", response),
            Err(err) => assert_eq!(err, Error::UnknownUnit(hash)),
        }
    }

    #[test]
    fn responds_to_existing_newest() {
        let (responder, mut store, keychains) = setup();
        let session_id = 2137;
        let units =
            random_full_parent_reconstrusted_units_up_to(5, NODE_COUNT, session_id, &keychains);
        for round_units in &units {
            for unit in round_units {
                store.insert(unit.clone());
            }
        }
        let requester = PeerId::new(1 as u8);
        let response = responder.handle_newest_unit_request(requester, rand::random(), &store);
        match response {
            DisseminationResponse::NewestUnit(newest_unit_response) => {
                newest_unit_response
                    .check(&keychains[NODE_ID.to_usize()])
                    .expect("should sign correctly");
            }
            other => panic!("Unexpected response: {:?}.", other),
        }
    }
}
