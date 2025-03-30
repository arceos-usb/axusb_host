use core::future::Future;

///host layer
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use futures::{future::BoxFuture, task::FutureObj};

use crate::{
    abstractions::{PlatformAbstractions, USBSystemConfig},
    event::EventBus,
};

use super::device::USBDevice;

pub trait Controller<'a, O, const RING_BUFFER_SIZE: usize>: Send + Sync
where
    O: PlatformAbstractions,
{
    fn new(
        config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
        event_bus: Arc<EventBus<'a, O, RING_BUFFER_SIZE>>,
    ) -> Self
    where
        Self: Sized;

    fn init(&self);

    fn device_accesses(&self) -> &Vec<Arc<USBDevice<O, RING_BUFFER_SIZE>>>;

    fn workaround(&'a self) -> BoxFuture<'a, ()>;
}

match_cfg! {
    #[cfg(feature = "backend-xhci")]=>{
        mod xhci;

        pub fn initialize_controller<'a, O,const RING_BUFFER_SIZE:usize>(
            config: Arc<USBSystemConfig<O,RING_BUFFER_SIZE>>,
            event_bus:Arc<EventBus<'a,O,RING_BUFFER_SIZE>>
        ) -> Box<dyn Controller<'a, O,RING_BUFFER_SIZE>>
        where
        //wtf
            O: PlatformAbstractions+'static,
            'a:'static,
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

impl<'a, O, const RING_BUFFER_SIZE: usize> Controller<'a, O, RING_BUFFER_SIZE> for DummyController
where
    O: PlatformAbstractions,
{
    fn new(
        _config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
        _evtbus: Arc<EventBus<'a, O, RING_BUFFER_SIZE>>,
    ) -> Self
    where
        Self: Sized,
    {
        panic!("dummy controller")
    }

    fn init(&self) {
        panic!("dummy controller")
    }

    fn device_accesses(&self) -> &Vec<Arc<USBDevice<O, RING_BUFFER_SIZE>>> {
        panic!("dummy controller")
    }

    fn workaround(&'a self) -> BoxFuture<'a, ()> {
        panic!("dummy controller")
    }
}
