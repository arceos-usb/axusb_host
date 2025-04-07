#![no_std]
#![feature(
    allocator_api,
    let_chains,
    exclusive_wrapper,
    ptr_as_ref_unchecked,
    fn_traits,
    sync_unsafe_cell,
    future_join,
    never_type
)]

#[macro_use(match_cfg)]
extern crate match_cfg;

use abstractions::{PlatformAbstractions, USBSystemConfig};
use alloc::{
    boxed::Box,
    collections::btree_map::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use async_lock::{OnceCell, RwLock};
use driver::driverapi::USBSystemDriverModule;
use embassy_futures::block_on;
use event::EventBus;
use futures::{
    future::{join, join_all},
    join, FutureExt,
};
use host::controllers::Controller;
use lazy_static::lazy_static;
use log::{info, trace};
use usb::functional_interface::USBLayer;
use usb_descriptor_decoder::DescriptorDecoder;

extern crate alloc;

pub mod abstractions;
pub mod driver;
pub mod event;
mod host;
pub mod usb;

pub struct USBSystem<'a, O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions + 'static,
    'a: 'static,
{
    config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
    controller: Box<dyn Controller<'a, O, RING_BUFFER_SIZE>>,
    usb_layer: USBLayer<'a, O, RING_BUFFER_SIZE>,
    event_bus: Arc<EventBus<'a, O, RING_BUFFER_SIZE>>,
    desc_decoder: Arc<RwLock<DescriptorDecoder>>,
}

impl<'a, O, const RING_BUFFER_SIZE: usize> USBSystem<'a, O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions + 'static,
    'a: 'static,
{
    pub fn new(config: USBSystemConfig<O, RING_BUFFER_SIZE>) -> Self {
        let config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>> = config.into();
        let event_bus = Arc::new(EventBus::new());
        let controller: Box<dyn Controller<'a, O, RING_BUFFER_SIZE>> =
            host::controllers::initialize_controller(config.clone(), event_bus.clone());
        let usb_layer = USBLayer::new(config.clone(), event_bus.clone());

        let mut usbsystem = USBSystem {
            config,
            controller,
            usb_layer,
            event_bus,
            desc_decoder: Arc::new(RwLock::new(DescriptorDecoder::new())),
        };

        #[cfg(feature = "packed-drivers")]
        {
            usbsystem.plug_driver_module(
                "hid-mouse".to_string(),
                Box::new(driver::implemented_drivers::hid_mouse::HIDMouseModule {}),
            );
        }

        usbsystem
    }

    pub fn plug_driver_module(
        &mut self,
        name: String,
        mut module: Box<dyn driver::driverapi::USBSystemDriverModule<'a, O, RING_BUFFER_SIZE>>,
    ) -> &mut Self {
        module.as_mut().preload_module(); //add some hooks?
        self.usb_layer.driver_modules.insert(name, module);

        self
    }

    pub fn stage_1_start_controller(&'a self) -> &Self {
        self.event_bus.pre_initialize_device.subscribe(|dev| {
            trace!("adding decoder ref to device!");
            block_on(dev.add_decoder(self.desc_decoder.clone()));
            squeak::Response::StaySubscribed
        });
        self.controller.init();
        info!("controller init complete!");
        self
    }

    pub fn stage_2_initialize_usb_layer(&'a self) -> &'a Self {
        self.event_bus.post_initialized_device.subscribe(|dev| {
            self.usb_layer.new_device_initialized(dev);
            squeak::Response::StaySubscribed
        });

        //TODO: more, like device descruction.etc

        info!("usb layer init complete!");
        self
    }

    async fn inner_stage_3_initial_controller_polling_and_deivces(&self) {
        join_all(
            self.controller
                .device_accesses()
                .iter()
                .map(|device| {
                    device.request_assign().then(|_| async {
                        self.event_bus
                            .post_initialized_device
                            .broadcast(device.clone());
                    })
                })
                .collect::<Vec<_>>(),
        )
        .await;

        info!("controller poll and initial device init complete!");
    }

    pub async fn async_run(&'a self) {
        //TODO structure run logic
        // join(self.controller.workaround(), self.usb_layer.workaround()).await
        info!("usb system workaround...");
        join(
            self.inner_stage_3_initial_controller_polling_and_deivces(),
            self.controller.workaround(),
        )
        .await;
    }

    pub fn block_run(&'a self) {
        block_on(self.async_run())
    }
}
