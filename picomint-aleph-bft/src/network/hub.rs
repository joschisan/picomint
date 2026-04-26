use crate::{
    alerts::AlertMessage,
    network::{NetworkData, NetworkDataInner, UnitMessage},
    Data, Network, Receiver, Recipient, Sender, Terminator,
};
use futures::{FutureExt, StreamExt};
use log::{debug, error, warn};

pub struct Hub<D: Data, N: Network<NetworkData<D>>> {
    network: N,
    units_to_send: Receiver<(UnitMessage<D>, Recipient)>,
    units_received: Sender<UnitMessage<D>>,
    alerts_to_send: Receiver<(AlertMessage<D>, Recipient)>,
    alerts_received: Sender<AlertMessage<D>>,
}

impl<D: Data, N: Network<NetworkData<D>>> Hub<D, N> {
    pub fn new(
        network: N,
        units_to_send: Receiver<(UnitMessage<D>, Recipient)>,
        units_received: Sender<UnitMessage<D>>,
        alerts_to_send: Receiver<(AlertMessage<D>, Recipient)>,
        alerts_received: Sender<AlertMessage<D>>,
    ) -> Self {
        Hub {
            network,
            units_to_send,
            units_received,
            alerts_to_send,
            alerts_received,
        }
    }

    fn send(&self, data: NetworkData<D>, recipient: Recipient) {
        self.network.send(data, recipient);
    }

    fn handle_incoming(&self, network_data: NetworkData<D>) {
        let NetworkData(network_data) = network_data;
        use NetworkDataInner::*;
        match network_data {
            Units(unit_message) => {
                if let Err(e) = self.units_received.unbounded_send(unit_message) {
                    warn!(target: "AlephBFT-network-hub", "Error when sending units to consensus {:?}", e);
                }
            }

            Alert(alert_message) => {
                if let Err(e) = self.alerts_received.unbounded_send(alert_message) {
                    warn!(target: "AlephBFT-network-hub", "Error when sending alerts to consensus {:?}", e);
                }
            }
        }
    }

    pub async fn run(mut self, mut terminator: Terminator) {
        loop {
            use NetworkDataInner::*;
            futures::select! {
                unit_message = self.units_to_send.next() => match unit_message {
                    Some((unit_message, recipient)) => self.send(NetworkData(Units(unit_message)), recipient),
                    None => {
                        error!(target: "AlephBFT-network-hub", "Outgoing units stream closed.");
                        break;
                    }
                },
                alert_message = self.alerts_to_send.next() => match alert_message {
                    Some((alert_message, recipient)) => self.send(NetworkData(Alert(alert_message)), recipient),
                    None => {
                        error!(target: "AlephBFT-network-hub", "Outgoing alerts stream closed.");
                        break;
                    }
                },
                incoming_message = self.network.next_event().fuse() => match incoming_message {
                    Some(incoming_message) => self.handle_incoming(incoming_message),
                    None => {
                        error!(target: "AlephBFT-network-hub", "Network stopped working.");
                        break;
                    }
                },
                _ = terminator.get_exit().fuse() => {
                    terminator.terminate_sync().await;
                    break;
                }
            }
        }

        debug!(target: "AlephBFT-network-hub", "Network ended.");
    }
}
