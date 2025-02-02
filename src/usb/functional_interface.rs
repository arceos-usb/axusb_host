use alloc::{boxed::Box, collections::btree_map::BTreeMap, string::String, sync::Arc, vec::Vec};
use async_lock::{OnceCell, RwLock};
use embassy_executor::{
    raw::{Executor, TaskPool},
    Spawner,
};
use embassy_futures::join::JoinArray;
use futures::{
    future::{BoxFuture, SelectOk},
    task::Spawn,
};
use usb_descriptor_decoder::descriptors::desc_device::Device;

use crate::{
    abstractions::{PlatformAbstractions, USBSystemConfig},
    driver,
    event::EventBus,
    host::device::USBDevice,
};

pub struct USBLayer<'a, O>
where
    O: PlatformAbstractions,
    [(); O::RING_BUFFER_SIZE]:,
{
    config: Arc<USBSystemConfig<O>>,
    eventbus: Arc<EventBus<'a, O>>,
    pub driver_modules: BTreeMap<String, Box<dyn driver::driverapi::USBSystemDriverModule<'a, O>>>,
    pub functional_interfaces: RwLock<
        BTreeMap<
            &'a str,
            Vec<
                Arc<
                    RwLock<
                        dyn driver::driverapi::USBSystemDriverModuleInstanceFunctionalInterface<
                            'a,
                            O,
                        >,
                    >,
                >,
            >,
        >,
    >,
}

impl<'a, O> USBLayer<'a, O>
where
    'a: 'static,
    O: PlatformAbstractions + 'static,
    [(); O::RING_BUFFER_SIZE]:,
{
    pub fn new(config: Arc<USBSystemConfig<O>>, evt_bus: Arc<EventBus<'a, O>>) -> Self {
        let usblayer = Self {
            config,
            driver_modules: BTreeMap::new(),
            functional_interfaces: BTreeMap::new().into(),
            eventbus: evt_bus,
        };
        usblayer
    }

    pub fn new_device_initialized(&self, device: &Arc<USBDevice<'a, O>>)
    where
        [(); O::RING_BUFFER_SIZE]:,
    {
        self.driver_modules
            .values()
            .filter_map(|module| {
                module
                    .should_active(&device, &self.config)
                    .map(|a| (a, module.name()))
            })
            .for_each(|(function, name)| {
                //todo!
                embassy_futures::block_on(self.functional_interfaces.write())
                    .entry(name)
                    .or_insert(Vec::new())
                    .push(function);
            });
    }
}
