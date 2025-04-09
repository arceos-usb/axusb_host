use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;
use alloc::{string::String, sync::Arc};
use async_lock::RwLock;
use async_trait::async_trait;
use embassy_futures::select;
use futures::task::FutureObj;

use crate::{
    abstractions::{PlatformAbstractions, USBSystemConfig},
    host::device::USBDevice,
};

pub trait USBSystemDriverModule<'a, O, const RING_BUFFER_SIZE: usize>: Send + Sync
where
    O: PlatformAbstractions,
{
    fn should_active(
        &self,
        device: Arc<USBDevice<O, RING_BUFFER_SIZE>>,
        config: &Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
    ) -> Option<Arc<RwLock<dyn USBSystemDriverModuleInstanceFunctionalInterface<'a, O>>>>;

    fn preload_module(&self);

    fn name(&self) -> &'a str;
}

pub trait USBSystemDriverModuleInstanceFunctionalInterface<'a, O>: Send + Sync
where
    O: PlatformAbstractions,
{
    fn run(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>>;
    fn pre_drop(&'a self);
}
