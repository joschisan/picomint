use crate::{
    backup::{BackupSink, BackupSource},
    Data, DataProvider, FinalizationHandler, OrderedUnit, UnitFinalizationHandler,
};
use std::marker::PhantomData;

/// This adapter allows to map an implementation of [`FinalizationHandler`] onto implementation of [`UnitFinalizationHandler`].
pub struct FinalizationHandlerAdapter<FH, D> {
    finalization_handler: FH,
    _phantom: PhantomData<D>,
}

impl<FH, D> From<FH> for FinalizationHandlerAdapter<FH, D> {
    fn from(value: FH) -> Self {
        Self {
            finalization_handler: value,
            _phantom: PhantomData,
        }
    }
}

impl<D: Data, FH: FinalizationHandler<D>> UnitFinalizationHandler
    for FinalizationHandlerAdapter<FH, D>
{
    type Data = D;

    fn batch_finalized(&mut self, batch: Vec<OrderedUnit<Self::Data>>) {
        for unit in batch {
            if let Some(data) = unit.data {
                self.finalization_handler.data_finalized(data)
            }
        }
    }
}

/// The local interface of the consensus algorithm. Bundles the data
/// provider, the unit finalization handler, and the typed backup sink/source
/// pair used to durably persist in-progress units across crashes.
#[derive(Clone)]
pub struct LocalIO<DP: DataProvider, UFH: UnitFinalizationHandler, US, UL> {
    data_provider: DP,
    finalization_handler: UFH,
    unit_saver: US,
    unit_loader: UL,
}

impl<
        DP: DataProvider,
        FH: FinalizationHandler<DP::Output>,
        US: BackupSink<DP::Output>,
        UL: BackupSource<DP::Output>,
    > LocalIO<DP, FinalizationHandlerAdapter<FH, DP::Output>, US, UL>
{
    /// Create a new local interface. Uses the simplified, recommended
    /// finalization handler that only deals with ordered data.
    pub fn new(
        data_provider: DP,
        finalization_handler: FH,
        unit_saver: US,
        unit_loader: UL,
    ) -> Self {
        Self {
            data_provider,
            finalization_handler: finalization_handler.into(),
            unit_saver,
            unit_loader,
        }
    }
}

impl<
        DP: DataProvider,
        UFH: UnitFinalizationHandler,
        US: BackupSink<UFH::Data>,
        UL: BackupSource<UFH::Data>,
    > LocalIO<DP, UFH, US, UL>
{
    /// Create a new local interface, providing a full implementation of a
    /// [`UnitFinalizationHandler`]. Implementing [`UnitFinalizationHandler`]
    /// directly is more complex and should be unnecessary for most usecases.
    /// Implement [`FinalizationHandler`] and use `new` instead, unless you
    /// absolutely know what you are doing.
    pub fn new_with_unit_finalization_handler(
        data_provider: DP,
        finalization_handler: UFH,
        unit_saver: US,
        unit_loader: UL,
    ) -> Self {
        Self {
            data_provider,
            finalization_handler,
            unit_saver,
            unit_loader,
        }
    }

    /// Disassemble the interface into components.
    pub fn into_components(self) -> (DP, UFH, US, UL) {
        let LocalIO {
            data_provider,
            finalization_handler,
            unit_saver,
            unit_loader,
        } = self;
        (data_provider, finalization_handler, unit_saver, unit_loader)
    }
}
