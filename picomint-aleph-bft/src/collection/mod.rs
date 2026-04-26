use crate::{
    config::DelaySchedule,
    network::UnitMessageTo,
    units::{UncheckedSignedUnit, Validator},
    Data, Keychain, PeerId, Receiver, Round, Sender, Signable, Signature, UncheckedSigned,
};
use futures::{channel::oneshot, Future};
use picomint_encoding::{Decodable, Encodable};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
};

mod service;

pub use service::{Collection, IO};

const LOG_TARGET: &str = "AlephBFT-collection";

/// Salt uniquely identifying an initial unit collection instance.
pub type Salt = u64;

fn generate_salt() -> Salt {
    let mut hasher = DefaultHasher::new();
    std::time::Instant::now().hash(&mut hasher);
    hasher.finish()
}

/// A response to the request for the newest unit.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Decodable, Encodable)]
pub struct NewestUnitResponse<D: Data> {
    requester: PeerId,
    responder: PeerId,
    unit: Option<UncheckedSignedUnit<D>>,
    salt: Salt,
}

impl<D: Data> Signable for NewestUnitResponse<D> {
    type Hash = Vec<u8>;

    fn hash(&self) -> Self::Hash {
        self.consensus_encode_to_vec()
    }
}

impl<D: Data> crate::Index for NewestUnitResponse<D> {
    fn index(&self) -> PeerId {
        self.responder
    }
}

impl<D: Data> NewestUnitResponse<D> {
    /// Create a newest unit response.
    pub fn new(
        requester: PeerId,
        responder: PeerId,
        unit: Option<UncheckedSignedUnit<D>>,
        salt: Salt,
    ) -> Self {
        NewestUnitResponse {
            requester,
            responder,
            unit,
            salt,
        }
    }

    /// The data included in this message, i.e. contents of the unit if any.
    pub fn included_data(&self) -> Vec<D> {
        match &self.unit {
            Some(u) => u.as_signable().included_data(),
            None => Vec::new(),
        }
    }
}

pub type CollectionResponse<D> = UncheckedSigned<NewestUnitResponse<D>, Signature>;

#[cfg(feature = "initial_unit_collection")]
pub fn initial_unit_collection<'a, D: Data>(
    keychain: &'a Keychain,
    validator: &'a Validator,
    messages_for_network: Sender<UnitMessageTo<D>>,
    starting_round_sender: oneshot::Sender<Option<Round>>,
    starting_round_from_backup: Round,
    responses_from_network: Receiver<CollectionResponse<D>>,
    request_delay: DelaySchedule,
) -> Result<impl Future<Output = ()> + 'a, ()> {
    let collection = Collection::new(keychain, validator);

    let collection = IO::new(
        starting_round_sender,
        starting_round_from_backup,
        responses_from_network,
        messages_for_network,
        collection,
        request_delay,
    );
    Ok(collection.run())
}

/// A trivial start that doesn't actually perform the initial unit collection.
#[cfg(not(feature = "initial_unit_collection"))]
pub fn initial_unit_collection<D: Data>(
    _keychain: &Keychain,
    _validator: &Validator,
    _messages_for_network: Sender<UnitMessageTo<D>>,
    starting_round_sender: oneshot::Sender<Option<Round>>,
    starting_round_from_backup: Round,
    _responses_from_network: Receiver<CollectionResponse<D>>,
    _request_delay: DelaySchedule,
) -> Result<impl Future<Output = ()>, ()> {
    if let Err(e) = starting_round_sender.send(Some(starting_round_from_backup)) {
        log::error!(target: LOG_TARGET, "Unable to send the starting round: {}", e);
        return Err(());
    }
    Ok(async {})
}
