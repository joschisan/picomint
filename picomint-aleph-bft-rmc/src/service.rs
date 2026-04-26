//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
use crate::{
    handler::{Handler, OnStartRmcResponse},
    scheduler::TaskScheduler,
    Message,
};
pub use aleph_bft_crypto::{Multisigned, Signable};
use core::fmt::Debug;
use log::{debug, warn};
use std::hash::Hash;

const LOG_TARGET: &str = "AlephBFT-rmc";

/// Reliable Multicast Box
///
/// The instance of [`Service<H, SCH>`] is used to reliably broadcast hashes of type `H`.
/// It collects the signed hashes and upon receiving a large enough number of them it yields
/// the multisigned hash.
///
/// A node with an instance of [`Service<H, SCH>`] can initiate broadcasting a message `msg: H`
/// by calling [`Service::start_rmc`]. As a result, the node signs `msg` and starts scheduling
/// messages for broadcast which can be obtained by awaiting on [`Service::next_message`]. When
/// sufficiently many nodes initiate rmc with the same message `msg` and a node collects enough
/// signatures to form a complete multisignature under the message, [`Service::process_message`]
/// will return the multisigned hash.
pub struct Service<H, SCH>
where
    H: Signable + Hash,
    SCH: TaskScheduler<Message<H>>,
{
    scheduler: SCH,
    handler: Handler<H>,
}

impl<H, SCH> Service<H, SCH>
where
    H: Signable + Hash + Eq + Clone + Debug,
    SCH: TaskScheduler<Message<H>>,
{
    pub fn new(scheduler: SCH, handler: Handler<H>) -> Self {
        Service { scheduler, handler }
    }

    /// Signs the given `hash` and adds the signature to the collection. If the given `hash`
    /// completes the multisignature, it is scheduled for the broadcasts and then returned.
    /// If the multisignature is not completed, `None` is returned. If the multisignature was
    /// already completed when starting rmc, no tasks are scheduled. Otherwise the signed hash
    /// is scheduled for the broadcasts.
    pub fn start_rmc(&mut self, hash: H) -> Option<Multisigned<H>> {
        debug!(target: LOG_TARGET, "starting rmc for {:?}", hash);
        match self.handler.on_start_rmc(hash) {
            OnStartRmcResponse::SignedHash(signed_hash) => {
                self.scheduler
                    .add_task(Message::SignedHash(signed_hash.into_unchecked()));
            }
            OnStartRmcResponse::MultisignedHash(multisigned) => {
                self.scheduler.add_task(Message::MultisignedHash(
                    multisigned.clone().into_unchecked(),
                ));
                return Some(multisigned);
            }
            OnStartRmcResponse::Noop => {}
        }
        None
    }

    /// Processes a message which can be of two types. If the message is a hash signed by one
    /// person, it adds it to the collective signature. If it completes the multisignature, it is
    /// returned. Otherwise `None` is returned. If the message is a multisigned hash, it returns
    /// the multisignature, if we haven't seen it before. Otherwise `None` is returned.
    pub fn process_message(&mut self, message: Message<H>) -> Option<Multisigned<H>> {
        match message {
            Message::SignedHash(unchecked) => match self.handler.on_signed_hash(unchecked) {
                Ok(Some(multisigned)) => {
                    self.scheduler.add_task(Message::MultisignedHash(
                        multisigned.clone().into_unchecked(),
                    ));
                    return Some(multisigned);
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(target: LOG_TARGET, "failed handling multisigned hash: {}", error);
                }
            },
            Message::MultisignedHash(unchecked) => {
                match self.handler.on_multisigned_hash(unchecked) {
                    Ok(Some(multisigned)) => {
                        self.scheduler.add_task(Message::MultisignedHash(
                            multisigned.clone().into_unchecked(),
                        ));
                        return Some(multisigned);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(target: LOG_TARGET, "failed handling signed hash: {}", error);
                    }
                }
            }
        }
        None
    }

    /// Obtain the next message scheduled for broadcast.
    pub async fn next_message(&mut self) -> Message<H> {
        self.scheduler.next_task().await
    }
}

#[cfg(test)]
mod tests {
    use crate::{DoublingDelayScheduler, Handler, Message, Service};
    use aleph_bft_crypto::{Multisigned, NumPeers, PeerId, Signed};
    use aleph_bft_mock::{bad_keychain, keychain, Signable};
    use futures::{
        channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
        future, StreamExt,
    };
    use rand::Rng;
    use std::{collections::HashMap, time::Duration};

    type TestMessage = Message<Signable>;

    struct TestEnvironment {
        rmc_services: Vec<Service<Signable, DoublingDelayScheduler<TestMessage>>>,
        rmc_start_tx: UnboundedSender<(Signable, PeerId)>,
        rmc_start_rx: UnboundedReceiver<(Signable, PeerId)>,
        broadcast_tx: UnboundedSender<(TestMessage, PeerId)>,
        broadcast_rx: UnboundedReceiver<(TestMessage, PeerId)>,
        hashes: HashMap<PeerId, Multisigned<Signable>>,
        message_filter: Box<dyn FnMut(PeerId, TestMessage) -> bool>,
    }

    enum EnvironmentEvent {
        NetworkMessage(TestMessage, PeerId),
        ManualBroadcast(TestMessage, PeerId),
        StartRmc(Signable, PeerId),
    }

    impl TestEnvironment {
        fn new(
            node_count: NumPeers,
            message_filter: impl FnMut(PeerId, TestMessage) -> bool + 'static,
        ) -> Self {
            let mut rmc_services = vec![];
            let (rmc_start_tx, rmc_start_rx) = unbounded();
            let (broadcast_tx, broadcast_rx) = unbounded();

            for i in 0..node_count.total() {
                let service = Service::new(
                    DoublingDelayScheduler::new(Duration::from_millis(1)),
                    Handler::new(keychain(node_count, PeerId::new(i as u8))),
                );
                rmc_services.push(service);
            }

            TestEnvironment {
                rmc_services,
                rmc_start_tx,
                rmc_start_rx,
                broadcast_tx,
                broadcast_rx,
                hashes: HashMap::new(),
                message_filter: Box::new(message_filter),
            }
        }

        fn start_rmc(&self, hash: Signable, node_index: PeerId) {
            self.rmc_start_tx
                .unbounded_send((hash, node_index))
                .expect("our channel should be open");
        }

        fn broadcast_message(&self, message: TestMessage, node_index: PeerId) {
            self.broadcast_tx
                .unbounded_send((message, node_index))
                .expect("our channel should be open");
        }

        fn handle_message(&mut self, message: TestMessage, node_index: PeerId, use_filter: bool) {
            for (j, service) in self.rmc_services.iter_mut().enumerate() {
                if j == node_index.to_usize()
                    || (use_filter
                        && !(self.message_filter)(PeerId::from(j as u8), message.clone()))
                {
                    continue;
                }
                if let Some(multisigned) = service.process_message(message.clone()) {
                    assert_eq!(self.hashes.insert(PeerId::from(j as u8), multisigned), None);
                    // there should be only one multisig per node
                }
            }
        }

        async fn next_event(&mut self) -> EnvironmentEvent {
            let message_futures = self
                .rmc_services
                .iter_mut()
                .map(|serv| Box::pin(serv.next_message()));
            tokio::select! {
                (message, i, _) = future::select_all(message_futures) => {
                    EnvironmentEvent::NetworkMessage(message, PeerId::new(i as u8))
                }
                maybe_message = self.broadcast_rx.next() => {
                    let (message, node_index) = maybe_message.expect("our channel should be open");
                    EnvironmentEvent::ManualBroadcast(message, node_index)
                }
                maybe_start_rmc = self.rmc_start_rx.next() => {
                    let (hash, node_index) = maybe_start_rmc.expect("our channel should be open");
                    EnvironmentEvent::StartRmc(hash, node_index)
                }
            }
        }

        async fn collect_multisigned_hashes(
            mut self,
            expected_multisigs: usize,
        ) -> HashMap<PeerId, Multisigned<Signable>> {
            while self.hashes.len() < expected_multisigs {
                match self.next_event().await {
                    EnvironmentEvent::StartRmc(hash, node_index) => {
                        let service = self
                            .rmc_services
                            .get_mut(node_index.to_usize())
                            .expect("service should exist");
                        if let Some(multisigned) = service.start_rmc(hash) {
                            assert_eq!(self.hashes.insert(node_index, multisigned), None);
                            // there should be only one multisig per node
                        }
                    }
                    EnvironmentEvent::NetworkMessage(message, node_index) => {
                        self.handle_message(message, node_index, true);
                    }
                    EnvironmentEvent::ManualBroadcast(message, node_index) => {
                        self.handle_message(message, node_index, false);
                    }
                }
            }
            self.hashes
        }
    }

    /// Create 10 honest nodes and let each of them start rmc for the same hash.
    #[tokio::test]
    async fn simple_scenario() {
        let node_count = NumPeers::new(10 as usize);
        let environment = TestEnvironment::new(node_count, |_, _| true);
        let hash: Signable = "56".into();
        for i in 0..node_count.total() {
            environment.start_rmc(hash.clone(), PeerId::new(i as u8));
        }

        let hashes = environment
            .collect_multisigned_hashes(node_count.total())
            .await;
        assert_eq!(hashes.len(), node_count.total());
        for i in 0..node_count.total() {
            let multisignature = &hashes[&PeerId::from(i as u8)];
            assert_eq!(multisignature.as_signable(), &hash);
        }
    }

    /// Each message is delivered with 20% probability
    #[tokio::test]
    async fn faulty_network() {
        let node_count = NumPeers::new(10 as usize);
        let mut rng = rand::thread_rng();
        let environment = TestEnvironment::new(node_count, move |_, _| rng.gen_range(0..5) == 0);

        let hash: Signable = "56".into();
        for i in 0..node_count.total() {
            environment.start_rmc(hash.clone(), PeerId::new(i as u8));
        }

        let hashes = environment
            .collect_multisigned_hashes(node_count.total())
            .await;
        assert_eq!(hashes.len(), node_count.total());
        for i in 0..node_count.total() {
            let multisignature = &hashes[&PeerId::from(i as u8)];
            assert_eq!(multisignature.as_signable(), &hash);
        }
    }

    /// Only 7 nodes start rmc and one of the nodes which didn't start rmc
    /// is delivered only messages with complete multisignatures
    #[tokio::test]
    async fn node_hearing_only_multisignatures() {
        let node_count = NumPeers::new(10 as usize);
        let environment = TestEnvironment::new(node_count, move |node_ix, message| {
            !matches!(
                (node_ix.to_usize(), message),
                (0, TestMessage::SignedHash(_))
            )
        });

        let threshold = node_count.threshold();
        let hash: Signable = "56".into();
        for i in 0..threshold {
            environment.start_rmc(hash.clone(), PeerId::new(i as u8));
        }

        let hashes = environment
            .collect_multisigned_hashes(node_count.total())
            .await;
        assert_eq!(hashes.len(), node_count.total());
        for i in 0..node_count.total() {
            let multisignature = &hashes[&PeerId::from(i as u8)];
            assert_eq!(multisignature.as_signable(), &hash);
        }
    }

    /// 7 honest nodes and 3 dishonest nodes which emit bad signatures and multisignatures
    #[tokio::test]
    async fn bad_signatures_and_multisignatures_are_ignored() {
        let node_count = NumPeers::new(10 as usize);
        let environment = TestEnvironment::new(node_count, |_, _| true);

        let bad_hash: Signable = "65".into();
        let bad_kc = bad_keychain(node_count, 0.into());
        let bad_msg =
            TestMessage::SignedHash(Signed::sign_with_index(bad_hash.clone(), &bad_kc).into());
        environment.broadcast_message(bad_msg, PeerId::new(0 as u8));
        let bad_msg = TestMessage::MultisignedHash(
            Signed::sign_with_index(bad_hash.clone(), &bad_kc)
                .into_partially_multisigned(&bad_kc)
                .into_unchecked(),
        );
        environment.broadcast_message(bad_msg, PeerId::new(0 as u8));

        let hash: Signable = "56".into();
        for i in 0..node_count.total() {
            environment.start_rmc(hash.clone(), PeerId::new(i as u8));
        }

        let hashes = environment
            .collect_multisigned_hashes(node_count.total())
            .await;
        assert_eq!(hashes.len(), node_count.total());
        for i in 0..node_count.total() {
            let multisignature = &hashes[&PeerId::from(i as u8)];
            assert_eq!(multisignature.as_signable(), &hash);
        }
    }
}
