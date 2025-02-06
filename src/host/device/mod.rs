use core::mem;

use alloc::{borrow::ToOwned, sync::Arc};

use async_lock::{OnceCell, RwLock, Semaphore, SemaphoreGuardArc};
use async_ringbuf::{traits::AsyncProducer, AsyncRb};
use futures::{channel::oneshot, FutureExt};
use log::{debug, info, trace};
use nosy::Sink;
use usb_descriptor_decoder::descriptors::{
    parser::RawDescriptorParser, topological_desc::TopologicalUSBDescriptorRoot,
    USBStandardDescriptorTypes,
};

use crate::{
    abstractions::{dma::DMA, PlatformAbstractions, USBSystemConfig},
    usb::{
        operations::{
            // construct_keep_callback_listener,
            control::{
                bRequest, bRequestStandard, bmRequestType, construct_control_transfer_type,
                ControlTransfer, DataTransferType, Recipient,
            },
            ChannelNumber,
            CompleteAction,
            Direction,
            ExtraAction,
            RequestResult,
            RequestedOperation,
            USBRequest,
        },
        standards::TopologyRoute,
    },
};

pub struct USBDevice<O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions,
{
    pub config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
    pub state: Arc<RwLock<DeviceState>>,
    pub slot_id: OnceCell<u8>,
    pub vendor_id: OnceCell<u16>,
    pub product_id: OnceCell<u16>,
    pub descriptor: OnceCell<Arc<TopologicalUSBDescriptorRoot>>,
    pub topology_path: TopologyRoute,
    configure_sem: Arc<Semaphore>,
    request_channel: RwLock<ArcAsyncRingBufPord<USBRequest, RING_BUFFER_SIZE>>,
}

pub enum DeviceState {
    Probed,
    Assigned,
    Configured,
    PreDrop, //for hot plug,  how to design drop all working functions mechanism?
}

pub type ArcAsyncRingBufPord<T, const N: usize> = async_ringbuf::wrap::AsyncWrap<
    Arc<AsyncRb<ringbuf::storage::Owning<[mem::MaybeUninit<T>; N]>>>,
    true,
    false,
>;
pub type ArcAsyncRingBufCons<T, const N: usize> = async_ringbuf::wrap::AsyncWrap<
    Arc<AsyncRb<ringbuf::storage::Owning<[mem::MaybeUninit<T>; N]>>>,
    false,
    true,
>;

#[derive(Debug)]
pub struct ConfigureSemaphore(SemaphoreGuardArc);

impl<O, const RING_BUFFER_SIZE: usize> USBDevice<O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
{
    pub fn new(
        cfg: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
        sender: ArcAsyncRingBufPord<USBRequest, RING_BUFFER_SIZE>,
    ) -> Self {
        USBDevice {
            state: RwLock::new(DeviceState::Probed).into(),
            vendor_id: OnceCell::new(),
            product_id: OnceCell::new(),
            descriptor: OnceCell::new(),
            request_channel: sender.into(),
            configure_sem: Semaphore::new(1).into(),
            topology_path: TopologyRoute::new(),
            slot_id: OnceCell::new(),
            config: cfg,
        }
    }

    pub fn acquire_cfg_sem(&self) -> Option<ConfigureSemaphore> {
        self.configure_sem.try_acquire_arc().map(ConfigureSemaphore)
    }

    async fn post_usb_request(&self, request: USBRequest) {
        self.request_channel
            .write()
            .await
            .push(request)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "device channel droped, wtf? vendor-{:?}|product-{:?}",
                    self.vendor_id.get(),
                    self.product_id.get()
                )
            })
    }

    pub async fn request_no_response(&self, request: RequestedOperation) {
        self.check_self_status().await;
        self.post_usb_request(USBRequest {
            operation: request,
            extra_action: ExtraAction::default(),
            complete_action: CompleteAction::default(),
        })
        .await;
    }

    ///I must lost my mind...
    pub async fn request_once(&self, request: RequestedOperation) -> Result<RequestResult, u8> {
        self.check_self_status().await;
        let (sender, receiver) = oneshot::channel();
        self.post_usb_request(USBRequest {
            extra_action: ExtraAction::NOOP,
            operation: request,
            complete_action: CompleteAction::SimpleResponse(sender),
        })
        .await;

        receiver.await.unwrap().to_owned()
    }

    // pub async fn keep_request(
    //     &self,
    //     request: RequestedOperation,
    //     channel_number: ChannelNumber,
    // ) -> Sink<Result<RequestResult, u8>> {
    //     self.check_self_status().await;
    //     let channel = construct_keep_callback_listener();
    //     self.post_usb_request(USBRequest {
    //         extra_action: ExtraAction::KeepFill(channel_number),
    //         operation: request,
    //         complete_action: CompleteAction::KeepResponse(channel.0),
    //     })
    //     .await;
    //     channel.1
    // }

    async fn check_self_status(&self) -> &Self {
        match *self.state.read().await {
            DeviceState::Probed => {
                self.request_assign().await;
            }
            DeviceState::PreDrop => todo!(),
            _ => (),
        };

        self
    }

    pub async fn request_assign(&self) {
        info!("device request assign!");
        {
            let sem = self.configure_sem.acquire_arc().await;
            self.post_usb_request(USBRequest {
                operation: RequestedOperation::InitializeDevice(self.topology_path.clone()),
                extra_action: ExtraAction::default(),
                complete_action: CompleteAction::DropSem(ConfigureSemaphore(sem)),
            })
            .await;
        }
        trace!("device initialize complete, now request device desc...");

        {
            let sem = self.configure_sem.acquire_arc().await;
            *self.state.write().await = DeviceState::Assigned;

            let (num_of_configs, mut parser) = {
                let buffer_device =
                    DMA::new_vec(0u8, O::PAGE_SIZE, O::PAGE_SIZE, self.config.os.dma_alloc());

                {
                    self.post_usb_request(USBRequest {
                        operation: RequestedOperation::Control(ControlTransfer {
                            request_type: bmRequestType::new(
                                Direction::In,
                                DataTransferType::Standard,
                                Recipient::Device,
                            ),
                            request: bRequest::Standard(bRequestStandard::GetDescriptor),
                            index: 0,
                            value: construct_control_transfer_type(
                                USBStandardDescriptorTypes::Device as u8,
                                0,
                            )
                            .bits(),
                            data: Some(buffer_device.addr_len_tuple()),
                            response: false,
                        }),
                        extra_action: ExtraAction::default(),
                        complete_action: CompleteAction::DropSem(ConfigureSemaphore(sem)),
                    })
                    .await;
                    let sem = self.configure_sem.acquire().await;
                    drop(sem);
                }

                let mut parser = RawDescriptorParser::new(buffer_device.to_owned());
                parser.single_state_cycle();
                (parser.num_of_configs(), parser)
            };

            let mut sem = self.configure_sem.acquire_arc().await;
            for index in 0..num_of_configs {
                let buffer =
                    DMA::new_vec(0u8, O::PAGE_SIZE, O::PAGE_SIZE, self.config.os.dma_alloc());
                self.post_usb_request(USBRequest {
                    operation: RequestedOperation::Control(ControlTransfer {
                        request_type: bmRequestType::new(
                            Direction::In,
                            DataTransferType::Standard,
                            Recipient::Device,
                        ),
                        request: bRequest::Standard(bRequestStandard::GetDescriptor),
                        index: 0,
                        value: construct_control_transfer_type(
                            USBStandardDescriptorTypes::Device as u8,
                            index as _,
                        )
                        .bits(),
                        data: Some(buffer.addr_len_tuple()),
                        response: false,
                    }),
                    extra_action: ExtraAction::default(),
                    complete_action: CompleteAction::DropSem(ConfigureSemaphore(sem)),
                })
                .await;
                sem = self.configure_sem.acquire_arc().await;
                parser.append_config(buffer.to_owned());
            }
            trace!("try to parse device descriptor!");
            self.descriptor
                .set(parser.summarize().into())
                .await
                .unwrap();
            debug!("parsed device desc: {:#?}", self.descriptor)
        };
    }
}
