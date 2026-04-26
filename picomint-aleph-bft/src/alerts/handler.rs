use crate::{
    alerts::{Alert, AlertMessage, ForkProof, ForkingNotification},
    units::Unit,
    Data, Keychain, Multisigned, PeerId, Recipient, SessionId, Signable, Signature, Signed,
    UncheckedSigned, UnitHash,
};
use aleph_bft_rmc::Message as RmcMessage;
use aleph_bft_types::Round;
use picomint_encoding::{Decodable, Encodable};
use std::{
    collections::{HashMap, HashSet},
    fmt::{Display, Formatter},
};

#[derive(Debug, PartialEq)]
pub enum Error {
    // commitment validity errors
    IncorrectlySignedUnit(PeerId),
    SameRound(Round, PeerId),
    WrongCreator(PeerId),
    // fork validity errors
    DifferentRounds(PeerId),
    SingleUnit(PeerId),
    WrongSession(PeerId),
    // other errors
    IncorrectlySignedAlert,
    RepeatedAlert(PeerId, PeerId),
    UnknownAlertRequest,
    UnknownAlertRMC,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IncorrectlySignedUnit(sender) => write!(f, "Incorrect commitment from {:?}: Some unit is incorrectly signed", sender),
            Error::SameRound(round, sender) => write!(f, "Incorrect commitment from {:?}: Two or more alerted units have the same round {:?}", sender, round),
            Error::WrongCreator(sender) => write!(f, "Incorrect commitment from {:?}: Some unit has a wrong creator", sender),
            Error::DifferentRounds(sender) => write!(f, "Incorrect fork alert from {:?}: Forking units come from different rounds", sender),
            Error::SingleUnit(sender) => write!(f, "Incorrect fork alert from {:?}: Two copies of a single unit do not constitute a fork", sender),
            Error::WrongSession(sender) => write!(f, "Incorrect fork alert from {:?}: Wrong session", sender),
            Error::IncorrectlySignedAlert => write!(f, "Received an incorrectly signed alert"),
            Error::RepeatedAlert(forker, sender) => write!(f, "We already know about an alert by {:?} about {:?}", sender, forker),
            Error::UnknownAlertRequest => write!(f, "Received a request for an unknown alert"),
            Error::UnknownAlertRMC => write!(f, "Completed an RMC for an unknown alert"),
        }
    }
}

type KnownAlerts<D> = HashMap<UnitHash, Signed<Alert<D>>>;

pub type OnOwnAlertResponse<D> = (AlertMessage<D>, Recipient, UnitHash);

pub type OnNetworkAlertResponse<D> = (Option<ForkingNotification<D>>, UnitHash);

type OnAlertRequestResponse<D> = (UncheckedSigned<Alert<D>, Signature>, Recipient);

#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub enum RmcResponse {
    RmcMessage(RmcMessage<UnitHash>),
    AlertRequest(UnitHash, Recipient),
    Noop,
}

/// The component responsible for fork alerts in AlephBFT.
pub struct Handler<D: Data> {
    session_id: SessionId,
    keychain: Keychain,
    known_forkers: HashMap<PeerId, ForkProof<D>>,
    known_alerts: KnownAlerts<D>,
    known_rmcs: HashMap<(PeerId, PeerId), UnitHash>,
}

impl<D: Data> Handler<D> {
    pub fn new(keychain: Keychain, session_id: SessionId) -> Self {
        Self {
            session_id,
            keychain,
            known_forkers: HashMap::new(),
            known_alerts: HashMap::new(),
            known_rmcs: HashMap::new(),
        }
    }

    fn is_forker(&self, forker: PeerId) -> bool {
        self.known_forkers.contains_key(&forker)
    }

    fn on_new_forker_detected(&mut self, forker: PeerId, proof: ForkProof<D>) {
        self.known_forkers.insert(forker, proof);
    }

    // Correctness rules:
    // 1) All units must be created by forker
    // 2) All units must come from different rounds
    // 3) There must be fewer of them than the maximum defined in the configuration.
    fn verify_commitment(&self, alert: &Alert<D>) -> Result<(), Error> {
        let mut rounds = HashSet::new();
        for u in &alert.legit_units {
            let u = match u.clone().check(&self.keychain) {
                Ok(u) => u,
                Err(_) => return Err(Error::IncorrectlySignedUnit(alert.sender)),
            };
            let full_unit = u.as_signable();
            if full_unit.creator() != alert.forker() {
                return Err(Error::WrongCreator(alert.sender));
            }
            if rounds.contains(&full_unit.round()) {
                return Err(Error::SameRound(full_unit.round(), alert.sender));
            }
            rounds.insert(full_unit.round());
        }
        Ok(())
    }

    fn verify_fork(&self, alert: &Alert<D>) -> Result<(), Error> {
        let (u1, u2) = &alert.proof;
        let (u1, u2) = {
            let u1 = u1.clone().check(&self.keychain);
            let u2 = u2.clone().check(&self.keychain);
            match (u1, u2) {
                (Ok(u1), Ok(u2)) => (u1, u2),
                _ => return Err(Error::IncorrectlySignedUnit(alert.sender)),
            }
        };
        let full_unit1 = u1.as_signable();
        let full_unit2 = u2.as_signable();
        if full_unit1.session_id() != self.session_id || full_unit2.session_id() != self.session_id
        {
            return Err(Error::WrongSession(alert.sender));
        }
        if full_unit1 == full_unit2 {
            return Err(Error::SingleUnit(alert.sender));
        }
        if full_unit1.creator() != full_unit2.creator() {
            return Err(Error::WrongCreator(alert.sender));
        }
        if full_unit1.round() != full_unit2.round() {
            return Err(Error::DifferentRounds(alert.sender));
        }
        Ok(())
    }

    /// Registers the RMC but does not actually send it; the returned hash must be passed to `start_rmc()` separately
    fn rmc_alert(&mut self, forker: PeerId, alert: Signed<Alert<D>>) -> UnitHash {
        let hash = alert.as_signable().hash();
        self.known_rmcs
            .insert((alert.as_signable().sender, forker), hash);
        self.known_alerts.insert(hash, alert);
        hash
    }

    /// Registers RMCs and messages but does not actually send them; make sure the returned values are forwarded to IO
    pub fn on_own_alert(&mut self, alert: Alert<D>) -> OnOwnAlertResponse<D> {
        let forker = alert.forker();
        self.known_forkers.insert(forker, alert.proof.clone());
        let alert = Signed::sign(alert, &self.keychain);
        let hash = self.rmc_alert(forker, alert.clone());
        (
            AlertMessage::ForkAlert(alert.into_unchecked()),
            Recipient::Everyone,
            hash,
        )
    }

    /// May return a `ForkingNotification`, which should be propagated
    pub fn on_network_alert(
        &mut self,
        alert: UncheckedSigned<Alert<D>, Signature>,
    ) -> Result<OnNetworkAlertResponse<D>, Error> {
        let alert = match alert.check(&self.keychain) {
            Ok(alert) => alert,
            Err(_) => {
                return Err(Error::IncorrectlySignedAlert);
            }
        };
        let contents = alert.as_signable();
        self.verify_fork(contents)?;
        let forker = contents.forker();
        let sender = alert.as_signable().sender;
        if self.known_rmcs.contains_key(&(contents.sender, forker)) {
            self.known_alerts.insert(contents.hash(), alert);
            return Err(Error::RepeatedAlert(sender, forker));
        }
        let maybe_notification = if self.is_forker(forker) {
            None
        } else {
            // We learn about this forker for the first time, need to send our own alert
            self.on_new_forker_detected(forker, contents.proof.clone());
            Some(ForkingNotification::Forker(contents.proof.clone()))
        };
        let hash_for_rmc = self.rmc_alert(forker, alert);

        Ok((maybe_notification, hash_for_rmc))
    }

    pub fn on_rmc_message(&self, sender: PeerId, message: RmcMessage<UnitHash>) -> RmcResponse {
        let hash = message.hash();
        if let Some(alert) = self.known_alerts.get(hash) {
            let alert_id = (alert.as_signable().sender, alert.as_signable().forker());
            if self.known_rmcs.get(&alert_id) == Some(hash) || message.is_complete() {
                RmcResponse::RmcMessage(message)
            } else {
                RmcResponse::Noop
            }
        } else {
            RmcResponse::AlertRequest(*hash, Recipient::Peer(sender))
        }
    }

    pub fn on_alert_request(
        &self,
        node: PeerId,
        hash: UnitHash,
    ) -> Result<OnAlertRequestResponse<D>, Error> {
        match self.known_alerts.get(&hash) {
            Some(alert) => Ok((alert.clone().into_unchecked(), Recipient::Peer(node))),
            None => Err(Error::UnknownAlertRequest),
        }
    }

    /// May return a `ForkingNotification`, which should be propagated
    pub fn alert_confirmed(
        &mut self,
        multisigned: Multisigned<UnitHash>,
    ) -> Result<ForkingNotification<D>, Error> {
        let alert = match self.known_alerts.get(multisigned.as_signable()) {
            Some(alert) => alert.as_signable(),
            None => return Err(Error::UnknownAlertRMC),
        };
        let forker = alert.proof.0.as_signable().creator();
        self.known_rmcs.insert((alert.sender, forker), alert.hash());
        self.verify_commitment(alert)?;
        Ok(ForkingNotification::Units(alert.legit_units.clone()))
    }
}

#[cfg(test)]
mod tests {
    use crate::units::ControlHash;
    use crate::{
        alerts::{
            handler::{Error, Handler, RmcResponse},
            Alert, AlertMessage, ForkProof, ForkingNotification,
        },
        units::{FullUnit, PreUnit},
        PartiallyMultisigned, Recipient, Round,
    };
    use aleph_bft_mock::{keychain, Data};
    use aleph_bft_rmc::Message;
    use aleph_bft_types::{Keychain, NodeMap, NumPeers, PeerId, Signable, Signed};

    type TestForkProof = ForkProof<Data>;

    fn full_unit(
        n_members: NumPeers,
        node_id: PeerId,
        round: Round,
        variant: Option<u32>,
    ) -> FullUnit<Data> {
        FullUnit::new(
            PreUnit::new(
                node_id,
                round,
                ControlHash::new(&NodeMap::with_size(n_members)),
            ),
            variant,
            0,
        )
    }

    fn make_fork_proof(
        node_id: PeerId,
        kc: &Keychain,
        round: Round,
        n_members: NumPeers,
    ) -> TestForkProof {
        let unit_0 = full_unit(n_members, node_id, round, Some(0));
        let unit_1 = full_unit(n_members, node_id, round, Some(1));
        let signed_unit_0 = Signed::sign(unit_0, kc).into_unchecked();
        let signed_unit_1 = Signed::sign(unit_1, kc).into_unchecked();
        (signed_unit_0, signed_unit_1)
    }

    #[test]
    fn distributes_alert_from_units() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let mut this = Handler::new(own_kc.clone(), 0);
        let fork_proof = make_fork_proof(forker_index, &forker_kc, 0, n_members);
        let alert = Alert::new(own_index, fork_proof, vec![]);
        let alert_hash = Signable::hash(&alert);
        match this.on_own_alert(alert.clone()) {
            (AlertMessage::ForkAlert(_), Recipient::Everyone, h) => {
                assert_eq!(h, alert_hash);
            }
            other => panic!("unexpected response {:?}", other),
        }
    }

    #[test]
    fn reacts_to_correctly_incoming_alert() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(1 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let mut this = Handler::new(own_kc.clone(), 0);
        let fork_proof = make_fork_proof(forker_index, &forker_kc, 0, n_members);
        let alert = Alert::new(own_index, fork_proof.clone(), vec![]);
        let alert_hash = Signable::hash(&alert);
        let signed_alert = Signed::sign(alert, &own_kc).into_unchecked();
        assert_eq!(
            this.on_network_alert(signed_alert),
            Ok((Some(ForkingNotification::Forker(fork_proof)), alert_hash)),
        );
    }

    #[test]
    fn asks_about_unknown_alert() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let alerter_index = PeerId::new(1 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let alerter_kc = keychain(n_members, alerter_index);
        let forker_kc = keychain(n_members, forker_index);
        let this: Handler<Data> = Handler::new(own_kc, 0);
        let fork_proof = make_fork_proof(forker_index, &forker_kc, 0, n_members);
        let alert = Alert::new(alerter_index, fork_proof, vec![]);
        let alert_hash = Signable::hash(&alert);
        let signed_alert_hash = Signed::sign_with_index(alert_hash, &alerter_kc).into_unchecked();
        let response = this.on_rmc_message(alerter_index, Message::SignedHash(signed_alert_hash));
        assert_eq!(
            response,
            RmcResponse::AlertRequest(alert_hash, Recipient::Peer(alerter_index)),
        );
    }

    #[test]
    fn ignores_wrong_alert() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let mut this = Handler::new(own_kc.clone(), 0);
        let valid_unit = Signed::sign(full_unit(n_members, forker_index, 0, Some(0)), &forker_kc)
            .into_unchecked();
        let wrong_fork_proof = (valid_unit.clone(), valid_unit);
        let wrong_alert = Alert::new(own_index, wrong_fork_proof, vec![]);
        let signed_wrong_alert = Signed::sign(wrong_alert, &own_kc).into_unchecked();
        assert_eq!(
            this.on_network_alert(signed_wrong_alert),
            Err(Error::SingleUnit(own_index)),
        );
    }

    #[test]
    fn responds_to_alert_queries() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let mut this = Handler::new(own_kc.clone(), 0);
        let alert = Alert::new(
            own_index,
            make_fork_proof(forker_index, &forker_kc, 0, n_members),
            vec![],
        );
        let alert_hash = Signable::hash(&alert);
        let signed_alert = Signed::sign(alert, &own_kc).into_unchecked();
        this.on_network_alert(signed_alert.clone()).unwrap();
        for i in 1..n_members.total() {
            let node_id = PeerId::new(i as u8);
            assert_eq!(
                this.on_alert_request(node_id, alert_hash),
                Ok((signed_alert.clone(), Recipient::Peer(node_id))),
            );
        }
    }

    #[test]
    fn notifies_only_about_multisigned_alert() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let other_honest_node = PeerId::new(1 as u8);
        let double_committer = PeerId::new(5 as u8);
        let forker_index = PeerId::new(6 as u8);
        let kcs: Vec<_> = (0..n_members.total())
            .map(|i| keychain(n_members, PeerId::new(i as u8)))
            .collect();
        let mut this = Handler::new(kcs[own_index.to_usize()].clone(), 0);
        let fork_proof = make_fork_proof(forker_index, &kcs[forker_index.to_usize()], 0, n_members);
        let empty_alert = Alert::new(double_committer, fork_proof.clone(), vec![]);
        let empty_alert_hash = Signable::hash(&empty_alert);
        let signed_empty_alert =
            Signed::sign(empty_alert, &kcs[double_committer.to_usize()]).into_unchecked();
        let signed_empty_alert_hash =
            Signed::sign_with_index(empty_alert_hash, &kcs[double_committer.to_usize()])
                .into_unchecked();
        let multisigned_empty_alert_hash = signed_empty_alert_hash
            .check(&kcs[double_committer.to_usize()])
            .expect("the signature is correct")
            .into_partially_multisigned(&kcs[double_committer.to_usize()]);
        assert_eq!(
            this.on_network_alert(signed_empty_alert),
            Ok((
                Some(ForkingNotification::Forker(fork_proof.clone())),
                empty_alert_hash,
            )),
        );
        let message = Message::MultisignedHash(multisigned_empty_alert_hash.into_unchecked());
        assert_eq!(
            this.on_rmc_message(other_honest_node, message.clone()),
            RmcResponse::RmcMessage(message),
        );
        let forker_unit = fork_proof.0.clone();
        let nonempty_alert = Alert::new(double_committer, fork_proof, vec![forker_unit]);
        let nonempty_alert_hash = Signable::hash(&nonempty_alert);
        let signed_nonempty_alert =
            Signed::sign(nonempty_alert, &kcs[double_committer.to_usize()]).into_unchecked();
        let signed_nonempty_alert_hash =
            Signed::sign_with_index(nonempty_alert_hash, &kcs[double_committer.to_usize()])
                .into_unchecked();
        let mut multisigned_nonempty_alert_hash = signed_nonempty_alert_hash
            .check(&kcs[double_committer.to_usize()])
            .expect("the signature is correct")
            .into_partially_multisigned(&kcs[double_committer.to_usize()]);
        for i in 1..n_members.total() - 2 {
            let node_id = PeerId::new(i as u8);
            let signed_nonempty_alert_hash =
                Signed::sign_with_index(nonempty_alert_hash, &kcs[node_id.to_usize()])
                    .into_unchecked();
            multisigned_nonempty_alert_hash = multisigned_nonempty_alert_hash.add_signature(
                signed_nonempty_alert_hash
                    .check(&kcs[double_committer.to_usize()])
                    .expect("the signature is correct"),
                &kcs[double_committer.to_usize()],
            );
        }
        let message = Message::MultisignedHash(multisigned_nonempty_alert_hash.into_unchecked());
        assert_eq!(
            this.on_network_alert(signed_nonempty_alert),
            Err(Error::RepeatedAlert(double_committer, forker_index)),
        );
        assert_eq!(
            this.on_rmc_message(other_honest_node, message.clone()),
            RmcResponse::RmcMessage(message),
        );
    }

    #[test]
    fn ignores_insufficiently_multisigned_alert() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let other_honest_node = PeerId::new(1 as u8);
        let double_committer = PeerId::new(5 as u8);
        let forker_index = PeerId::new(6 as u8);
        let kcs: Vec<_> = (0..n_members.total())
            .map(|i| keychain(n_members, PeerId::new(i as u8)))
            .collect();
        let mut this = Handler::new(kcs[own_index.to_usize()].clone(), 0);
        let fork_proof = make_fork_proof(forker_index, &kcs[forker_index.to_usize()], 0, n_members);
        let empty_alert = Alert::new(double_committer, fork_proof.clone(), vec![]);
        let empty_alert_hash = Signable::hash(&empty_alert);
        let signed_empty_alert =
            Signed::sign(empty_alert, &kcs[double_committer.to_usize()]).into_unchecked();
        assert_eq!(
            this.on_network_alert(signed_empty_alert),
            Ok((
                Some(ForkingNotification::Forker(fork_proof.clone())),
                empty_alert_hash,
            )),
        );
        let forker_unit = fork_proof.0.clone();
        let nonempty_alert = Alert::new(double_committer, fork_proof, vec![forker_unit]);
        let nonempty_alert_hash = Signable::hash(&nonempty_alert);
        let signed_nonempty_alert =
            Signed::sign(nonempty_alert, &kcs[double_committer.to_usize()]).into_unchecked();
        let signed_nonempty_alert_hash =
            Signed::sign_with_index(nonempty_alert_hash, &kcs[double_committer.to_usize()])
                .into_unchecked();
        let mut multisigned_nonempty_alert_hash = signed_nonempty_alert_hash
            .check(&kcs[double_committer.to_usize()])
            .expect("the signature is correct")
            .into_partially_multisigned(&kcs[double_committer.to_usize()]);
        for i in 1..3 {
            let node_id = PeerId::new(i as u8);
            let signed_nonempty_alert_hash =
                Signed::sign_with_index(nonempty_alert_hash, &kcs[node_id.to_usize()])
                    .into_unchecked();
            multisigned_nonempty_alert_hash = multisigned_nonempty_alert_hash.add_signature(
                signed_nonempty_alert_hash
                    .check(&kcs[double_committer.to_usize()])
                    .expect("the signature is correct"),
                &kcs[double_committer.to_usize()],
            );
        }
        let message = Message::MultisignedHash(multisigned_nonempty_alert_hash.into_unchecked());
        assert_eq!(
            this.on_network_alert(signed_nonempty_alert),
            Err(Error::RepeatedAlert(double_committer, forker_index)),
        );
        assert_eq!(
            this.on_rmc_message(other_honest_node, message.clone()),
            RmcResponse::RmcMessage(message),
        );
    }

    #[test]
    fn verify_fork_ok() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let this: Handler<Data> = Handler::new(own_kc, 0);
        let fork_proof = make_fork_proof(forker_index, &forker_kc, 0, n_members);
        let alert = Alert::new(own_index, fork_proof, vec![]);
        assert_eq!(this.verify_fork(&alert), Ok(()));
    }

    #[test]
    fn verify_fork_wrong_session() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let this: Handler<Data> = Handler::new(own_kc, 1);
        let fork_proof = make_fork_proof(forker_index, &forker_kc, 0, n_members);
        let alert = Alert::new(own_index, fork_proof, vec![]);
        assert_eq!(
            this.verify_fork(&alert),
            Err(Error::WrongSession(own_index))
        );
    }

    #[test]
    fn verify_fork_different_creators() {
        let n_members = NumPeers::new(7 as usize);
        let kcs: Vec<_> = (0..n_members.total())
            .map(|i| keychain(n_members, PeerId::new(i as u8)))
            .collect();
        let this: Handler<Data> = Handler::new(kcs[0].clone(), 0);
        let fork_proof = {
            let unit_0 = full_unit(n_members, PeerId::new(6 as u8), 0, Some(0));
            let unit_1 = full_unit(n_members, PeerId::new(5 as u8), 0, Some(0));
            let signed_unit_0 = Signed::sign(unit_0, &kcs[6]).into_unchecked();
            let signed_unit_1 = Signed::sign(unit_1, &kcs[5]).into_unchecked();
            (signed_unit_0, signed_unit_1)
        };
        let sender = PeerId::new(0 as u8);
        let alert = Alert::new(sender, fork_proof, vec![]);
        assert_eq!(this.verify_fork(&alert), Err(Error::WrongCreator(sender)));
    }

    #[test]
    fn verify_fork_different_rounds() {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(0 as u8);
        let forker_index = PeerId::new(6 as u8);
        let own_kc = keychain(n_members, own_index);
        let forker_kc = keychain(n_members, forker_index);
        let this: Handler<Data> = Handler::new(own_kc, 0);
        let fork_proof = {
            let unit_0 = full_unit(n_members, forker_index, 0, Some(0));
            let unit_1 = full_unit(n_members, forker_index, 1, Some(0));
            let signed_unit_0 = Signed::sign(unit_0, &forker_kc).into_unchecked();
            let signed_unit_1 = Signed::sign(unit_1, &forker_kc).into_unchecked();
            (signed_unit_0, signed_unit_1)
        };
        let alert = Alert::new(own_index, fork_proof, vec![]);
        assert_eq!(
            this.verify_fork(&alert),
            Err(Error::DifferentRounds(own_index))
        );
    }

    #[test]
    fn alert_confirmed_out_of_the_blue() {
        alert_confirmed(false, true);
    }

    #[test]
    fn alert_confirmed_bad_commitment() {
        alert_confirmed(true, false);
    }

    #[test]
    fn alert_confirmed_correct() {
        alert_confirmed(true, true);
    }

    fn alert_confirmed(make_known: bool, good_commitment: bool) {
        let n_members = NumPeers::new(7 as usize);
        let own_index = PeerId::new(1 as u8);
        let forker_index = PeerId::new(6 as u8);
        let kcs: Vec<_> = (0..n_members.total())
            .map(|i| keychain(n_members, PeerId::new(i as u8)))
            .collect();
        let mut this = Handler::new(kcs[own_index.to_usize()].clone(), 0);
        let fork_proof = if good_commitment {
            make_fork_proof(forker_index, &kcs[forker_index.to_usize()], 0, n_members)
        } else {
            let unit_0 = full_unit(n_members, forker_index, 0, Some(0));
            let unit_1 = full_unit(n_members, forker_index, 1, Some(1));
            let signed_unit_0 =
                Signed::sign(unit_0, &kcs[forker_index.to_usize()]).into_unchecked();
            let signed_unit_1 =
                Signed::sign(unit_1, &kcs[forker_index.to_usize()]).into_unchecked();
            (signed_unit_0, signed_unit_1)
        };
        let alert = Alert::new(own_index, fork_proof, vec![]);
        let alert_hash = Signable::hash(&alert);
        let signed_alert = Signed::sign(alert, &kcs[own_index.to_usize()]).into_unchecked();
        if make_known {
            let _ = this.on_network_alert(signed_alert);
        }
        let signed_alert_hash =
            Signed::sign_with_index(alert_hash, &kcs[own_index.to_usize()]).into_unchecked();
        let mut multisigned_alert_hash = signed_alert_hash
            .check(&kcs[forker_index.to_usize()])
            .expect("the signature is correct")
            .into_partially_multisigned(&kcs[own_index.to_usize()]);
        for i in 1..n_members.total() - 1 {
            let node_id = PeerId::new(i as u8);
            let signed_alert_hash =
                Signed::sign_with_index(alert_hash, &kcs[node_id.to_usize()]).into_unchecked();
            multisigned_alert_hash = multisigned_alert_hash.add_signature(
                signed_alert_hash
                    .check(&kcs[forker_index.to_usize()])
                    .expect("the signature is correct"),
                &kcs[forker_index.to_usize()],
            );
        }
        assert!(multisigned_alert_hash.is_complete());
        let multisigned_alert_hash = match multisigned_alert_hash {
            PartiallyMultisigned::Complete { multisigned } => multisigned,
            PartiallyMultisigned::Incomplete { .. } => unreachable!(),
        };
        let expected = match (make_known, good_commitment) {
            (true, true) => Ok(ForkingNotification::Units(vec![])),
            (true, false) => Err(Error::UnknownAlertRMC),
            (false, true) => Err(Error::UnknownAlertRMC),
            (false, false) => Err(Error::UnknownAlertRMC),
        };
        assert_eq!(this.alert_confirmed(multisigned_alert_hash), expected);
    }
}
