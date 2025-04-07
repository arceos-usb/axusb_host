use core::{cell::UnsafeCell, future::Future};

use alloc::{boxed::Box, collections::btree_map::BTreeMap, string::String, sync::Arc, vec::Vec};
use async_lock::{Mutex, OnceCell, RwLock};
use dynamic_join_array::DynamicJoinArray;
use embassy_futures::join::JoinArray;
use futures::{
    future::{BoxFuture, SelectOk},
    task::Spawn,
};
use log::{info, trace};
use usb_descriptor_decoder::descriptors::desc_device::Device;

use crate::{
    abstractions::{PlatformAbstractions, USBSystemConfig},
    driver::{
        self,
        driverapi::{USBSystemDriverModule, USBSystemDriverModuleInstanceFunctionalInterface},
    },
    event::EventBus,
    host::device::USBDevice,
};

pub struct USBLayer<'a, O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions,
{
    config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
    eventbus: Arc<EventBus<'a, O, RING_BUFFER_SIZE>>,
    pub driver_modules: BTreeMap<
        String,
        Box<dyn driver::driverapi::USBSystemDriverModule<'a, O, RING_BUFFER_SIZE>>,
    >,
    pub functional_interfaces: RwLock<
        BTreeMap<
            &'a str,
            Vec<(
                Arc<
                    RwLock<
                        dyn driver::driverapi::USBSystemDriverModuleInstanceFunctionalInterface<
                            'a,
                            O,
                        >,
                    >,
                >,
                usize,
            )>,
        >,
    >,
    pub dynamic_join_array: Arc<DynamicJoinArray>,
}

impl<'a, O, const RING_BUFFER_SIZE: usize> USBLayer<'a, O, RING_BUFFER_SIZE>
where
    'a: 'static,
    O: PlatformAbstractions + 'static,
{
    pub fn new(
        config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
        evt_bus: Arc<EventBus<'a, O, RING_BUFFER_SIZE>>,
    ) -> Self {
        let usblayer = Self {
            config,
            driver_modules: BTreeMap::new(),
            functional_interfaces: BTreeMap::new().into(),
            eventbus: evt_bus,
            dynamic_join_array: Arc::new(DynamicJoinArray::new().into()),
        };
        usblayer
    }

    pub fn new_device_initialized(&self, device: &Arc<USBDevice<O, RING_BUFFER_SIZE>>) {
        self.driver_modules
            .values()
            .filter_map(
                |module: &Box<dyn USBSystemDriverModule<'a, O, RING_BUFFER_SIZE>>| {
                    module
                        .should_active(&device, &self.config)
                        .map(|a| (a, module.name()))
                },
            )
            .for_each(|(function, name)| {
                //safety: feature holded ref would drop while module drop or device drop
                let future = unsafe {
                    (*(function.as_ref()
                        as *const RwLock<
                            dyn USBSystemDriverModuleInstanceFunctionalInterface<'a, O>,
                        >
                        as *mut RwLock<
                            dyn USBSystemDriverModuleInstanceFunctionalInterface<'a, O>,
                        >))
                        .get_mut()
                        .run()
                };

                trace!("setteled driver instance future!");
                let idx = embassy_futures::block_on(self.dynamic_join_array.add(future));
                trace!("setteled driver instance future!");
                embassy_futures::block_on(self.functional_interfaces.write())
                    .entry(name)
                    .or_insert(Vec::new())
                    .push((function, idx));
                trace!("placed instance into array!");
            });

        info!("initialized new device!");
    }

    pub async fn functional_interface_workaround(&self) {
        trace!("driver instance futures polling!");
        self.dynamic_join_array.work().await;
    }
}
