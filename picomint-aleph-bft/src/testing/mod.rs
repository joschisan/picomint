mod alerts;
mod behind;
mod byzantine;
mod crash;
mod crash_recovery;
mod creation;
mod dag;
mod unreliable;

use crate::{
    backup::mock::{MockSink, MockSource},
    create_config, run_session,
    units::UncheckedSignedUnit,
    Config, DelayConfig, LocalIO, Network as NetworkT, NumPeers, PeerId, SpawnHandle, TaskHandle,
    Terminator,
};
use aleph_bft_mock::{
    keychain, Data, DataProvider, FinalizationHandler, Network as MockNetwork,
    ReconnectSender as ReconnectSenderGeneric, Spawner,
};
use futures::channel::{mpsc::UnboundedReceiver, oneshot};
use parking_lot::Mutex;
use std::{sync::Arc, time::Duration};

pub type NetworkData = crate::NetworkData<Data>;

pub type Network = MockNetwork<NetworkData>;
pub type ReconnectSender = ReconnectSenderGeneric<NetworkData>;
pub type SavedUnits = Arc<Mutex<Vec<UncheckedSignedUnit<Data>>>>;

pub fn init_log() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::max())
        .is_test(true)
        .try_init();
}

pub fn gen_delay_config() -> DelayConfig {
    DelayConfig {
        tick_interval: Duration::from_millis(5),
        unit_rebroadcast_interval_min: Duration::from_millis(400),
        unit_rebroadcast_interval_max: Duration::from_millis(500),
        //50, 50, 50, 50, ...
        unit_creation_delay: Arc::new(|_| Duration::from_millis(50)),
        //100, 100, 100, ...
        coord_request_delay: Arc::new(|_| Duration::from_millis(100)),
        //3, 1, 1, 1, ...
        coord_request_recipients: Arc::new(|t| if t == 0 { 3 } else { 1 }),
        // 50, 50, 50, 50, ...
        parent_request_delay: Arc::new(|_| Duration::from_millis(50)),
        // 1, 1, 1, ...
        parent_request_recipients: Arc::new(|_| 1),
        // 50, 50, 50, 50, ...
        newest_request_delay: Arc::new(|_| Duration::from_millis(50)),
    }
}

pub fn gen_config(node_ix: PeerId, n_members: NumPeers, delay_config: DelayConfig) -> Config {
    create_config(n_members, node_ix, 0, 5000, delay_config, Duration::ZERO)
        .expect("Should always succeed with Duration::ZERO")
}

pub struct HonestMember {
    finalization_rx: UnboundedReceiver<Data>,
    saved_state: SavedUnits,
    exit_tx: oneshot::Sender<()>,
    handle: TaskHandle,
}

pub fn spawn_honest_member(
    spawner: Spawner,
    node_index: PeerId,
    n_members: NumPeers,
    units: Vec<UncheckedSignedUnit<Data>>,
    data_provider: DataProvider,
    network: impl 'static + NetworkT<NetworkData>,
) -> HonestMember {
    let (finalization_handler, finalization_rx) = FinalizationHandler::new();
    let config = gen_config(node_index, n_members, gen_delay_config());
    let (exit_tx, exit_rx) = oneshot::channel();
    let spawner_inner = spawner;
    let unit_loader = MockSource::new(units);
    let saved_state: SavedUnits = Arc::new(Mutex::new(Vec::new()));
    let unit_saver = MockSink::from_shared(saved_state.clone());
    let local_io = LocalIO::new(data_provider, finalization_handler, unit_saver, unit_loader);
    let member_task = async move {
        let kc = keychain(n_members, node_index);
        run_session(
            config,
            local_io,
            network,
            kc,
            spawner_inner,
            Terminator::create_root(exit_rx, "AlephBFT-member"),
        )
        .await
    };
    let handle = spawner.spawn_essential("member", member_task);
    HonestMember {
        finalization_rx,
        saved_state,
        exit_tx,
        handle,
    }
}
