use core::future::Future;

///host layer
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use futures::{future::BoxFuture, task::FutureObj};

use crate::{
    abstractions::{PlatformAbstractions, USBSystemConfig},
    event::EventBus,
};

use super::device::USBDevice;

pub trait Controller<'a, O>: Send + Sync
where
    O: PlatformAbstractions,
    [(); O::RING_BUFFER_SIZE]:,
{
    fn new(config: Arc<USBSystemConfig<O>>, event_bus: Arc<EventBus<'a, O>>) -> Self
    where
        Self: Sized;

    fn init(&self);

    /// each device should able to access actual transfer function in controller
    fn device_accesses(&self) -> &Vec<Arc<USBDevice<'a, O>>>;

    fn workaround(&'a self) -> BoxFuture<'a, ()>;
}

match_cfg! {
    #[cfg(feature = "backend-xhci")]=>{
        mod xhci;

        pub fn initialize_controller<'a, O>(
            config: Arc<USBSystemConfig<O>>,
            event_bus:Arc<EventBus<'a,O>>
        ) -> Box<dyn Controller<'a, O>>
        where
        //wtf
            O: PlatformAbstractions+'static,
            'a:'static,
             [(); O::RING_BUFFER_SIZE]:
        {
            Box::new(xhci::XHCIController::new(config,event_bus))
        }
    }
    _=>{
        pub fn initialize_controller<'a, O>(
            config: Arc<USBSystemConfig<O>>,
        ) -> Box<dyn Controller<'a, O>>
        where
            O: PlatformAbstractions+'static,
            'a:'static, [(); O::RING_BUFFER_SIZE]://wtf
        {
            Box::new(DummyController::new(config))
        }
    }
}

struct DummyController;

impl<'a, O> Controller<'a, O> for DummyController
where
    O: PlatformAbstractions,
    [(); O::RING_BUFFER_SIZE]:,
{
    fn new(_config: Arc<USBSystemConfig<O>>, _evtbus: Arc<EventBus<'a, O>>) -> Self
    where
        Self: Sized,
    {
        panic!("dummy controller")
    }

    fn init(&self) {
        panic!("dummy controller")
    }

    fn device_accesses(&self) -> &Vec<Arc<USBDevice<'a, O>>> {
        panic!("dummy controller")
    }

    fn workaround(&'a self) -> BoxFuture<'a, ()> {
        panic!("dummy controller")
    }
}
