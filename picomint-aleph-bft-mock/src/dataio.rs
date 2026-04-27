use aleph_bft_types::{DataProvider as DataProviderT, FinalizationHandler as FinalizationHandlerT};
use async_trait::async_trait;
use futures::{channel::mpsc::unbounded, future::pending};
use log::error;
use picomint_encoding::{Decodable, Encodable};

type Receiver<T> = futures::channel::mpsc::UnboundedReceiver<T>;
type Sender<T> = futures::channel::mpsc::UnboundedSender<T>;

pub type Data = u32;

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct DataProvider {
    counter: usize,
    n_data: Option<usize>,
}

impl DataProvider {
    pub fn new() -> Self {
        Self {
            counter: 0,
            n_data: None,
        }
    }

    pub fn new_finite(n_data: usize) -> Self {
        Self {
            counter: 0,
            n_data: Some(n_data),
        }
    }
    pub fn new_range(start: usize, end: usize) -> Self {
        Self {
            counter: start,
            n_data: Some(end),
        }
    }
}

#[async_trait]
impl DataProviderT for DataProvider {
    type Output = Data;

    async fn get_data(&mut self) -> Option<Data> {
        let result = self.counter as u32;
        self.counter += 1;
        if let Some(n_data) = self.n_data {
            if n_data < self.counter {
                return None;
            }
        }
        Some(result)
    }
}

#[derive(
    Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Decodable, Encodable,
)]
pub struct StalledDataProvider {}

impl StalledDataProvider {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl DataProviderT for StalledDataProvider {
    type Output = Data;

    async fn get_data(&mut self) -> Option<Data> {
        pending().await
    }
}

#[derive(Clone, Debug)]
pub struct FinalizationHandler {
    tx: Sender<Data>,
}

impl FinalizationHandlerT<Data> for FinalizationHandler {
    fn data_finalized(&mut self, data: Data) {
        if let Err(e) = self.tx.unbounded_send(data) {
            error!(target: "finalization-handler", "Error when sending data from FinalizationHandler {:?}.", e);
        }
    }
}

impl FinalizationHandler {
    pub fn new() -> (Self, Receiver<Data>) {
        let (tx, rx) = unbounded();

        (Self { tx }, rx)
    }
}
