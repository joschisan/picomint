use crate::{
    backup::BackupSink,
    dag::DagUnit,
    units::{UncheckedSignedUnit, WrappedUnit},
    Data, Receiver, Sender, Terminator,
};
use futures::{FutureExt, StreamExt};
use log::{debug, error};

const LOG_TARGET: &str = "AlephBFT-backup-saver";

/// Component responsible for saving units into backup.
pub struct BackupSaver<D: Data, S: BackupSink<D>> {
    units_from_consensus: Receiver<DagUnit<D>>,
    responses_for_consensus: Sender<DagUnit<D>>,
    backup: S,
}

impl<D: Data, S: BackupSink<D>> BackupSaver<D, S> {
    pub fn new(
        units_from_consensus: Receiver<DagUnit<D>>,
        responses_for_consensus: Sender<DagUnit<D>>,
        backup: S,
    ) -> BackupSaver<D, S> {
        BackupSaver {
            units_from_consensus,
            responses_for_consensus,
            backup,
        }
    }

    pub fn save_unit(&mut self, unit: &DagUnit<D>) -> std::io::Result<()> {
        let unit: UncheckedSignedUnit<D> = unit.clone().unpack().into();
        self.backup.save(unit)
    }

    pub async fn run(&mut self, mut terminator: Terminator) {
        let mut terminator_exit = false;
        loop {
            futures::select! {
                unit = self.units_from_consensus.next() => {
                    let item = match unit {
                        Some(unit) => unit,
                        None => {
                            error!(target: LOG_TARGET, "receiver of units to save closed early");
                            break;
                        },
                    };
                    if let Err(e) = self.save_unit(&item) {
                        error!(target: LOG_TARGET, "couldn't save item to backup: {:?}", e);
                        break;
                    }
                    if self.responses_for_consensus.unbounded_send(item).is_err() {
                        error!(target: LOG_TARGET, "couldn't respond with saved unit to consensus");
                        break;
                    }
                },
                _ = terminator.get_exit().fuse() => {
                    debug!(target: LOG_TARGET, "backup saver received exit signal.");
                    terminator_exit = true;
                }
            }

            if terminator_exit {
                debug!(target: LOG_TARGET, "backup saver decided to exit.");
                terminator.terminate_sync().await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::{
        channel::{mpsc, oneshot},
        StreamExt,
    };

    use aleph_bft_mock::{keychain, Data};

    use crate::{
        backup::{mock::MockSink, BackupSaver},
        dag::ReconstructedUnit,
        units::{creator_set, preunit_to_signed_unit, TestingSignedUnit},
        NumPeers, Terminator,
    };

    type TestUnit = ReconstructedUnit<TestingSignedUnit>;
    type TestBackupSaver = BackupSaver<Data, MockSink<Data>>;
    struct PrepareSaverResponse<F: futures::Future> {
        task: F,
        units_for_saver: mpsc::UnboundedSender<TestUnit>,
        units_from_saver: mpsc::UnboundedReceiver<TestUnit>,
        exit_tx: oneshot::Sender<()>,
    }

    fn prepare_saver() -> PrepareSaverResponse<impl futures::Future> {
        let (units_for_saver, units_from_consensus) = mpsc::unbounded();
        let (units_for_consensus, units_from_saver) = mpsc::unbounded();
        let (exit_tx, exit_rx) = oneshot::channel();
        let backup = MockSink::new();

        let task = {
            let mut saver: TestBackupSaver =
                BackupSaver::new(units_from_consensus, units_for_consensus, backup);

            async move {
                saver.run(Terminator::create_root(exit_rx, "saver")).await;
            }
        };

        PrepareSaverResponse {
            task,
            units_for_saver,
            units_from_saver,
            exit_tx,
        }
    }

    #[tokio::test]
    async fn test_proper_relative_responses_ordering() {
        let node_count = NumPeers::new(5_usize);
        let PrepareSaverResponse {
            task,
            units_for_saver,
            mut units_from_saver,
            exit_tx,
        } = prepare_saver();

        let handle = tokio::spawn(async {
            task.await;
        });

        let creators = creator_set(node_count);
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|id| keychain(node_count, id))
            .collect();
        let units: Vec<TestUnit> = node_count
            .peer_ids()
            .map(|id| {
                ReconstructedUnit::initial(preunit_to_signed_unit(
                    creators[id.to_usize()].create_unit(0).unwrap(),
                    0,
                    &keychains[id.to_usize()],
                ))
            })
            .collect();

        for u in units.iter() {
            units_for_saver.unbounded_send(u.clone()).unwrap();
        }

        for u in units {
            let u_backup = units_from_saver.next().await.unwrap();
            assert_eq!(u, u_backup);
        }

        exit_tx.send(()).unwrap();
        handle.await.unwrap();
    }
}
