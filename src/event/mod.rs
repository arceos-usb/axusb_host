use alloc::{boxed::Box, collections::btree_map::BTreeMap, sync::Arc};
use async_lock::RwLock;
use squeak::Delegate;

use crate::{
    abstractions::PlatformAbstractions,
    driver::{self, driverapi::USBSystemDriverModuleInstanceFunctionalInterface},
    host::device::USBDevice,
};

pub struct EventBus<'a, O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions,
{
    pub pre_initialize_device: Delegate<'a, Arc<USBDevice<O, RING_BUFFER_SIZE>>>,
    pub post_initialized_device: Delegate<'a, Arc<USBDevice<O, RING_BUFFER_SIZE>>>,
    pub pre_drop_device: Delegate<'a, Arc<USBDevice<O, RING_BUFFER_SIZE>>>,
    pub new_interface: Delegate<
        'a,
        (
            Box<dyn driver::driverapi::USBSystemDriverModule<'a, O, RING_BUFFER_SIZE>>,
            Arc<RwLock<dyn USBSystemDriverModuleInstanceFunctionalInterface<'a, O>>>,
        ),
    >,
}

unsafe impl<'a, O, const RING_BUFFER_SIZE: usize> Sync for EventBus<'a, O, RING_BUFFER_SIZE>
//safety: it wont mut
where
    O: PlatformAbstractions
{
}

unsafe impl<'a, O, const RING_BUFFER_SIZE: usize> Send for EventBus<'a, O, RING_BUFFER_SIZE> where
    O: PlatformAbstractions
{
}

impl<'a, O, const RING_BUFFER_SIZE: usize> EventBus<'a, O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
{
    pub fn new() -> Self {
        Self {
            post_initialized_device: Delegate::new(),
            pre_drop_device: Delegate::new(),
            new_interface: Delegate::new(),
            pre_initialize_device: Delegate::new(),
        }
    }
}
