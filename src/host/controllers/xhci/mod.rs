use core::{
    cell::SyncUnsafeCell,
    mem,
    num::NonZeroUsize,
    ops::DerefMut,
    sync::atomic::{fence, Ordering},
};

use ::futures::{stream, FutureExt, StreamExt};
use alloc::{borrow::ToOwned, collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use async_lock::{Mutex, OnceCell, RwLock};
use async_ringbuf::traits::{AsyncConsumer, AsyncProducer};
use axhid::hidreport::hid::Item;
use context::{DeviceContextList, ScratchpadBufferArray};
use embassy_futures::block_on;
use event_ring::EventRing;
use futures::channel::oneshot;
use inner_urb::XHCICompleteAction;
use log::{debug, error, info, trace, warn};
use ring::Ring;
use ringbuf::traits::{Consumer, Split};
use usb_descriptor_decoder::descriptors::USBStandardDescriptorTypes;
use xhci::{
    accessor::Mapper,
    context::{Input, InputHandler},
    extended_capabilities::XhciSupportedProtocol,
    ring::trb::{
        command::{self},
        event::{self, CommandCompletion, CompletionCode},
        transfer::{self, TransferType},
    },
};

use crate::{
    abstractions::{dma::DMA, PlatformAbstractions, USBSystemConfig},
    host::device::{ArcAsyncRingBufCons, USBDevice},
    usb::operations::{
        control::{
            bRequest, bRequestStandard, bmRequestType, construct_control_transfer_type,
            ControlTransfer, DataTransferType, Recipient,
        }, CompleteAction, Direction, RequestResult, USBRequest,
    },
};

use super::{controller_events::EventHandlerTable, Controller};

mod context;
mod event_ring;
mod inner_urb;
mod ring;

pub type RegistersBase = xhci::Registers<MemMapper>;
pub type RegistersExtList = xhci::extended_capabilities::List<MemMapper>;
pub type SupportedProtocol = XhciSupportedProtocol<MemMapper>;

const TAG: &str = "[XHCI]";

#[derive(Clone)]
pub struct MemMapper;
impl Mapper for MemMapper {
    unsafe fn map(&mut self, phys_start: usize, _bytes: usize) -> NonZeroUsize {
        NonZeroUsize::new_unchecked(phys_start)
    }
    fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
}

pub struct Receiver<const RINGBUF_SIZE: usize> {
    pub slot: OnceCell<u8>,
    pub receiver: ArcAsyncRingBufCons<USBRequest, RINGBUF_SIZE>,
}

pub struct XHCIController<'a, O>
//had to poll controller it self!
where
    'a: 'static,
    O: PlatformAbstractions + 'a,
    [(); O::RING_BUFFER_SIZE]:,
{
    config: Arc<USBSystemConfig<O>>,
    //safety:regs MUST exist in mem otherwise would panic when construct
    regs: SyncUnsafeCell<RegistersBase>,
    ext_list: Option<RegistersExtList>,
    max_slots: u8,
    max_ports: u8,
    max_irqs: u16,
    scratchpad_buf_arr: OnceCell<ScratchpadBufferArray<O>>,
    cmd: Mutex<Ring<O>>,
    event: EventRing<O>,
    dev_ctx: RwLock<DeviceContextList<O, { O::RING_BUFFER_SIZE }>>,
    devices: SyncUnsafeCell<Vec<Arc<USBDevice<'a, O>>>>,
    requests: SyncUnsafeCell<Vec<Receiver<{ O::RING_BUFFER_SIZE }>>>,
    finish_jobs: RwLock<BTreeMap<usize, XHCICompleteAction>>,
    extra_works: BTreeMap<usize, USBRequest>,
    event_tables: RwLock<EventHandlerTable<'a, O>>,
}

impl<'a, O> XHCIController<'a, O>
where
    'a: 'static,
    O: PlatformAbstractions + 'a,
    [(); O::RING_BUFFER_SIZE]:,
{
    fn chip_hardware_reset(&self) -> &Self {
        debug!("{TAG} Reset begin");
        debug!("{TAG} Stop");
        let regs = unsafe { self.regs.get().as_mut_unchecked() };

        regs.operational.usbcmd.update_volatile(|c| {
            c.clear_run_stop();
        });
        debug!("{TAG} Until halt");
        while !regs.operational.usbsts.read_volatile().hc_halted() {}
        debug!("{TAG} Halted");

        let o = &mut regs.operational;
        // debug!("xhci stat: {:?}", o.usbsts.read_volatile());

        debug!("{TAG} Wait for ready...");
        while o.usbsts.read_volatile().controller_not_ready() {}
        debug!("{TAG} Ready");

        o.usbcmd.update_volatile(|f| {
            f.set_host_controller_reset();
        });

        while o.usbcmd.read_volatile().host_controller_reset() {}

        debug!("{TAG} Reset HC");

        while regs
            .operational
            .usbcmd
            .read_volatile()
            .host_controller_reset()
            || regs
                .operational
                .usbsts
                .read_volatile()
                .controller_not_ready()
        {}

        info!("{TAG} XCHI reset ok");
        self
    }

    fn set_max_device_slots(&self) -> &Self {
        let max_slots = self.max_slots;
        debug!("{TAG} Setting enabled slots to {}.", max_slots);
        unsafe { self.regs.get().as_mut_unchecked() }
            .operational
            .config
            .update_volatile(|r| {
                r.set_max_device_slots_enabled(max_slots);
            });
        self
    }

    fn set_dcbaap(&self) -> &Self {
        let dcbaap = self
            .dev_ctx
            .try_read()
            .expect("should gurantee exclusive access here")
            .dcbaap();
        debug!("{TAG} Writing DCBAAP: {:X}", dcbaap);
        unsafe { self.regs.get().as_mut_unchecked() }
            .operational
            .dcbaap
            .update_volatile(|r| {
                r.set(dcbaap as u64);
            });
        self
    }

    fn set_cmd_ring(&self) -> &Self {
        let ring = self
            .cmd
            .try_lock()
            .expect("should gurantee exclusive access while initialize");
        let crcr = ring.register();
        let cycle = ring.cycle;

        debug!("{TAG} Writing CRCR: {:X}", crcr);
        unsafe { self.regs.get().as_mut_unchecked() }
            .operational
            .crcr
            .update_volatile(|r| {
                r.set_command_ring_pointer(crcr);
                if cycle {
                    r.set_ring_cycle_state();
                } else {
                    r.clear_ring_cycle_state();
                }
            });

        self
    }

    fn init_ir(&self) -> &Self {
        debug!("{TAG} Disable interrupts");
        let regs = unsafe { self.regs.get().as_mut_unchecked() };

        regs.operational.usbcmd.update_volatile(|r| {
            r.clear_interrupter_enable();
        });

        let mut ir0 = regs.interrupter_register_set.interrupter_mut(0);
        {
            debug!("{TAG} Writing ERSTZ");
            ir0.erstsz.update_volatile(|r| r.set(1));

            let erdp = self.event.erdp();
            debug!("{TAG} Writing ERDP: {:X}", erdp);

            ir0.erdp.update_volatile(|r| {
                r.set_event_ring_dequeue_pointer(erdp);
            });

            let erstba = self.event.erstba();
            debug!("{TAG} Writing ERSTBA: {:X}", erstba);

            ir0.erstba.update_volatile(|r| {
                r.set(erstba);
            });
            ir0.imod.update_volatile(|im| {
                im.set_interrupt_moderation_interval(0);
                im.set_interrupt_moderation_counter(0);
            });

            debug!("{TAG} Enabling primary interrupter.");
            ir0.iman.update_volatile(|im| {
                im.set_interrupt_enable();
            });
        }

        self
    }

    async fn setup_scratchpads(&self) -> &Self {
        let scratchpad_buf_arr = {
            let buf_count = {
                let count = unsafe { self.regs.get().as_mut_unchecked() }
                    .capability
                    .hcsparams2
                    .read_volatile()
                    .max_scratchpad_buffers();
                debug!("{TAG} Scratch buf count: {}", count);
                count
            };
            if buf_count == 0 {
                error!("buf count=0,is it a error?");
                return self;
            }
            let scratchpad_buf_arr = ScratchpadBufferArray::new(buf_count, self.config.os.clone());

            {
                let mut read_volatile = unsafe {
                    self.dev_ctx
                        .try_read()
                        .expect("should garantee exclusive access here")
                        .dcbaa
                        .get()
                        .read_volatile()
                };
                read_volatile[0] = scratchpad_buf_arr.register() as u64;
            }

            debug!(
                "{TAG} Setting up {} scratchpads, at {:#0x}",
                buf_count,
                scratchpad_buf_arr.register()
            );
            scratchpad_buf_arr
        };

        let _ = self.scratchpad_buf_arr.set(scratchpad_buf_arr).await;
        self
    }

    fn reset_ports(&self) -> &Self {
        //TODO: reset usb 3 port
        let regs = unsafe { self.regs.get().as_mut_unchecked() };
        let port_len = regs.port_register_set.len();

        for i in 0..port_len {
            debug!("{TAG} Port {} start reset", i,);
            regs.port_register_set.update_volatile_at(i, |port| {
                port.portsc.set_0_port_enabled_disabled();
                port.portsc.set_port_reset();
            });

            while regs
                .port_register_set
                .read_volatile_at(i)
                .portsc
                .port_reset()
            {}

            debug!("{TAG} Port {} reset ok", i);
        }
        self
    }

    fn initial_probe(&self) -> &Self {
        for (port_id, port) in unsafe { self.regs.get().as_mut_unchecked() }
            .port_register_set
            .into_iter() //safety: checked, is read_volatile
            .enumerate()
        {
            let portsc = port.portsc;
            info!(
                "{TAG} Port {}: Enabled: {}, Connected: {}, Speed {}, Power {}",
                port_id,
                portsc.port_enabled_disabled(),
                portsc.current_connect_status(),
                portsc.port_speed(),
                portsc.port_power()
            );

            if !portsc.current_connect_status() {
                // warn!("port {i} connected, but not enabled!");
                continue;
            }

            {
                use async_ringbuf::{traits::*, AsyncStaticRb};
                let (prod, cons) =
                    AsyncStaticRb::<USBRequest, { O::RING_BUFFER_SIZE }>::default().split();

                unsafe { self.devices.get().as_mut_unchecked() }.push(
                    {
                        let mut usbdevice = USBDevice::new(self.config.clone(), prod);
                        usbdevice
                            .topology_path
                            .append_port_number((port_id + 1) as _);
                        usbdevice
                    }
                    .into(),
                );
                unsafe { self.requests.get().as_mut_unchecked() }.push(Receiver {
                    slot: OnceCell::new(),
                    receiver: cons,
                });
            }
        }

        self
    }

    fn start(&self) -> &Self {
        let regs = unsafe { self.regs.get().as_mut_unchecked() };
        debug!("{TAG} Start run");
        regs.operational.usbcmd.update_volatile(|r| {
            r.set_run_stop();
        });

        while regs.operational.usbsts.read_volatile().hc_halted() {}

        info!("{TAG} Is running");

        regs.doorbell.update_volatile_at(0, |r| {
            r.set_doorbell_stream_id(0);
            r.set_doorbell_target(0);
        });

        self
    }

    // async fn test_cmd(&mut self) -> &mut Self {
    //     //TODO:assert like this in runtime if build with debug mode?
    //     debug!("{TAG} Test command ring");
    //     for _ in 0..3 {
    //         let completion = self
    //             .post_cmd(command::Allowed::Noop(command::Noop::new()))
    //             .unwrap();
    //     }
    //     debug!("{TAG} Command ring ok");
    //     self
    // }

    fn ring_db(&self, slot: u8, stream: Option<u16>, target: Option<u8>) {
        // might waste efficient? or actually low cost compare to actual transfer(in hardware)
        unsafe { self.regs.get().as_mut_unchecked() }
            .doorbell
            .update_volatile_at(slot as _, |r| {
                stream.map(|stream| r.set_doorbell_stream_id(stream));
                target.map(|target| r.set_doorbell_target(target));
            });
    }

    fn update_erdp(&self) {
        unsafe { self.regs.get().as_mut_unchecked() }
            .interrupter_register_set
            .interrupter_mut(0)
            .erdp
            .update_volatile(|f| {
                f.set_event_ring_dequeue_pointer(self.event.erdp());
            });
    }

    async fn post_command(&self, trb: command::Allowed) -> CommandCompletion {
        let addr = self.cmd.lock().await.enque_command(trb);
        let (sender, receiver) = oneshot::channel();

        self.finish_jobs
            .write()
            .await
            .insert(addr, XHCICompleteAction::CommandCallback(sender));

        self.ring_db(0, 0.into(), 0.into());
        fence(Ordering::Release);

        receiver.await.unwrap()
    }

    fn get_speed(&self, port: u8) -> u8 {
        unsafe { self.regs.get().as_mut_unchecked() }
            .port_register_set
            .read_volatile_at(port as _)
            .portsc
            .port_speed()
    }

    #[allow(unused_variables)]
    async fn on_event_arrived(&mut self) {
        while let Some((event, cycle)) = self.event.next() {
            debug!("{TAG}:[CMD] received event:{:?},cycle{cycle}", event);

            match event {
                event::Allowed::TransferEvent(transfer_event) => {
                    let addr = transfer_event.trb_pointer() as _;
                    //todo: transfer event trb had extra info compare to command event., should we split these two?

                    self.mark_transfer_completed(transfer_event.completion_code(), addr)
                        .await;
                }
                event::Allowed::CommandCompletion(command_completion) => {
                    let addr = command_completion.command_trb_pointer() as _;

                    self.mark_command_completed(addr, command_completion).await;
                }
                event::Allowed::PortStatusChange(port_status_change) => {
                    warn!("{TAG} port status changed! {:#?}", port_status_change);
                }
                event::Allowed::BandwidthRequest(bandwidth_request) => todo!(),
                event::Allowed::Doorbell(doorbell) => todo!(),
                event::Allowed::HostController(host_controller) => todo!(),
                event::Allowed::DeviceNotification(device_notification) => todo!(),
                event::Allowed::MfindexWrap(mfindex_wrap) => todo!(),
            }
        }

        self.update_erdp();
    }

    async fn mark_command_completed(&mut self, addr: usize, cmp: CommandCompletion) {
        //should compile to jump table?
        if self.finish_jobs.read().await.contains_key(&addr) {
            self.finish_jobs
                .write()
                .then(|mut write| async move { write.remove(&addr).unwrap() })
                .then(|action| async move {
                    match action {
                        XHCICompleteAction::CommandCallback(sender) => sender.send(cmp),
                        _ => {
                            panic!("do not call command completion on transfer event!")
                        }
                    }
                    .unwrap();
                })
                .await
        }
    }

    async fn mark_transfer_completed(&mut self, code: Result<CompletionCode, u8>, addr: usize) {
        //should compile to jump table?
        if self.finish_jobs.read().await.contains_key(&addr) {
            self.finish_jobs
                .write()
                .then(|mut write| async move { write.remove(&addr).unwrap() })
                .then(|action| async move {
                    match action {
                        XHCICompleteAction::STANDARD(CompleteAction::NOOP) => {}
                        XHCICompleteAction::STANDARD(CompleteAction::SimpleResponse(sender)) => {
                            let _ = sender.send(code.map(|a| a.into()).map_err(|a| a as _));
                        }
                        XHCICompleteAction::STANDARD(CompleteAction::DropSem(
                            configure_semaphore,
                        )) => {
                            match code.unwrap_or_else(|_| {
                                panic!("got fail signal on executing trb {:x}", addr)
                            }) {
                                CompletionCode::Success | CompletionCode::ShortPacket => {
                                    drop(configure_semaphore);
                                }
                                other => panic!(
                                    "got fail signal on executing trb {:x}-{:?}",
                                    addr, other
                                ),
                            }
                        }
                        _ => {
                            panic!("keep working request should not appear at here!")
                        }
                    };
                })
                .await
        } else if self.extra_works.contains_key(&addr) {
            match &mut self.extra_works.get_mut(&addr).unwrap().complete_action {
                CompleteAction::KeepResponse(async_wrap) => {
                    async_wrap.notify(&code.map(|a| a.into()).map_err(|a| a as _));
                }
                _ => panic!("oneshot job appeared at extra works zone!"),
            }
        }
    }

    async fn run_once(&mut self) {
        stream::select_all(unsafe {
            //TODO: rewrite this into yield stream. but not just post once
            self.requests.get().read_volatile().iter_mut().map(|r| {
                r.receiver
                    .pop()
                    .map(|res| res.map(|req| (req, &r.slot)))
                    .into_stream()
            })
        })
        .filter_map(|a| async move { a })
        .for_each(|a| self.post_transfer(a.0, a.1))
        .await;
    }

    #[allow(unused_variables)]
    async fn post_transfer(&self, req: USBRequest, slot: &OnceCell<u8>) {
        match req.operation {
            crate::usb::operations::RequestedOperation::Control(control_transfer) => {
                let key = self
                    .control_transfer(*unsafe { slot.get_unchecked() }, control_transfer)
                    .await;
                self.finish_jobs
                    .write()
                    .await
                    .insert(key, req.complete_action.into());
            }
            crate::usb::operations::RequestedOperation::Bulk(bulk_transfer) => todo!(),
            crate::usb::operations::RequestedOperation::Interrupt(interrupt_transfer) => todo!(),
            crate::usb::operations::RequestedOperation::Isoch(isoch_transfer) => todo!(),
            crate::usb::operations::RequestedOperation::InitializeDevice(route) => {
                let dev = unsafe { self.devices.get().as_ref_unchecked() }
                    .iter()
                    .find(|dev| dev.topology_path == route)
                    .unwrap_or_else(|| {
                        panic!(
                            "want assign a new device, but such device with route {} notfound",
                            route
                        )
                    });
                self.assign_address_device(dev).await;
                if let CompleteAction::DropSem(sem) = req.complete_action {
                    drop(sem);
                } else {
                    todo!("restruct urb system in xhci!")
                }
            }
            crate::usb::operations::RequestedOperation::NOOP => {
                debug!("{TAG}-device {:#?} transfer nope!", slot)
                //requestblock, in xhci, had additional request type "command request, thus we should create another URB but not just use USBRequest!"
                //TODO: enum XHCIRequest
            }
        }
    }

    async fn assign_address_device(&self, device: &Arc<USBDevice<'a, O>>) {
        const CONTROL_DCI: usize = 1;

        let slot_id = self.enable_slot().await;
        debug!("slot id acquired! {slot_id} for {}", device.topology_path);
        let _ = device.slot_id.set(slot_id).await;

        self.dev_ctx.write().await.new_slot(slot_id, 32); //TODO: basically, now a days all usb device  should had 32 endpoints, but for now let's just hardcode it...
        let port_speed = self.get_speed(device.topology_path.get_hub_index_at_tier(0));
        let default_max_packet_size = parse_default_max_packet_size_from_speed(port_speed);
        let context_addr = {
            let (control_channel_addr, cycle_bit) = {
                let _temp = self.dev_ctx.read().await;
                let ring = _temp.read_transfer_ring(slot_id, CONTROL_DCI).unwrap();
                (ring.register(), ring.cycle)
            };

            let mut writer = self.dev_ctx.write().await;
            let context_mut = writer
                .device_ctx_inners
                .get_mut(&slot_id)
                .unwrap()
                .in_ctx
                .deref_mut();

            let control_context = context_mut.control_mut();
            control_context.set_add_context_flag(0);
            control_context.set_add_context_flag(1);
            for i in 2..32 {
                control_context.clear_drop_context_flag(i);
            }

            let slot_context = context_mut.device_mut().slot_mut();
            slot_context.clear_multi_tt();
            slot_context.clear_hub();
            slot_context.set_route_string({
                let rs = device.topology_path.route_string();
                assert_eq!(rs, 0); // for now, not support more hub ,so hardcode as 0.//TODO: generate route string
                rs
            });
            slot_context.set_context_entries(1);
            slot_context.set_max_exit_latency(0);
            slot_context.set_root_hub_port_number(device.topology_path.port_number()); // use port number
            slot_context.set_number_of_ports(0);
            slot_context.set_parent_hub_slot_id(0);
            slot_context.set_tt_think_time(0);
            slot_context.set_interrupter_target(0);
            slot_context.set_speed(port_speed);

            let endpoint_0 = context_mut.device_mut().endpoint_mut(CONTROL_DCI);
            endpoint_0.set_endpoint_type(xhci::context::EndpointType::Control);
            endpoint_0.set_max_packet_size(default_max_packet_size);
            endpoint_0.set_max_burst_size(0);
            endpoint_0.set_error_count(3);
            endpoint_0.set_tr_dequeue_pointer(control_channel_addr);
            if cycle_bit {
                endpoint_0.set_dequeue_cycle_state();
            } else {
                endpoint_0.clear_dequeue_cycle_state();
            }
            endpoint_0.set_interval(0);
            endpoint_0.set_max_primary_streams(0);
            endpoint_0.set_mult(0);
            endpoint_0.set_error_count(3);

            (context_mut as *const Input<16>).addr() as u64
        };
        fence(Ordering::Release);
        {
            let request_result = self
                .post_command(command::Allowed::AddressDevice(
                    *command::AddressDevice::default()
                        .set_slot_id(slot_id)
                        .set_input_context_pointer(context_addr),
                ))
                .await;
            assert_eq!(
                RequestResult::Success,
                Into::<RequestResult>::into(request_result.completion_code().unwrap()),
                "address device failed! {:#?}",
                request_result
            );
        }
        fence(Ordering::Release);

        let actual_speed = {
            //set speed
            let buffer = DMA::new_vec(0u8, 8, 64, self.config.os.dma_alloc());
            let (sender, receiver) = oneshot::channel();

            self.post_transfer(
                USBRequest {
                    extra_action: Default::default(),
                    operation: crate::usb::operations::RequestedOperation::Control(
                        ControlTransfer {
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
                            data: Some((buffer.addr(), buffer.length_for_bytes())),
                            response: false,
                        },
                    ),
                    complete_action: CompleteAction::SimpleResponse(sender),
                },
                &device.slot_id,
            )
            .await;

            let request_result = receiver.await;
            if let Ok(Ok(RequestResult::Success)) = request_result {
            } else {
                panic!("get basic desc failed! {:#?}", request_result);
            }

            let mut data = [0u8; 8];
            data[..8].copy_from_slice(&buffer);
            trace!("got {:?}", data);
            data.last()
                .map(|len| if *len == 0 { 8u8 } else { *len })
                .unwrap()
        };

        let context_addr = {
            let mut writer = self.dev_ctx.write().await;
            let input = writer
                .device_ctx_inners
                .get_mut(&slot_id)
                .unwrap()
                .in_ctx
                .deref_mut();

            input
                .device_mut()
                .endpoint_mut(1) //dci=1: endpoint 0
                .set_max_packet_size(actual_speed as _);

            debug!(
                "CMD: evaluating context for set endpoint0 packet size {}",
                actual_speed
            );
            (input as *const Input<16>).addr() as _
        };

        fence(Ordering::Release);
        {
            let request_result = self
                .post_command(command::Allowed::EvaluateContext(
                    *command::EvaluateContext::default()
                        .set_slot_id(slot_id)
                        .set_input_context_pointer(context_addr),
                ))
                .await;

            assert_eq!(
                Into::<RequestResult>::into(request_result.completion_code().unwrap()),
                RequestResult::Success,
                "evaluate context failed! {:#?}",
                request_result
            );
        }
    }

    async fn enable_slot(&self) -> u8 {
        let request_result = self
            .post_command(command::Allowed::EnableSlot(
                *command::EnableSlot::default().set_slot_type({
                    // TODO: PCI未初始化，读不出来
                    // let mut regs = self.regs.lock();
                    // match regs.supported_protocol(port) {
                    //     Some(p) => p.header.read_volatile().protocol_slot_type(),
                    //     None => {
                    //         warn!(
                    //             "{TAG} Failed to find supported protocol information for port {}",
                    //             port
                    //         );
                    //         0
                    //     }
                    // }
                    0
                }),
            ))
            .await;

        assert_eq!(
            Into::<RequestResult>::into(request_result.completion_code().unwrap()),
            RequestResult::Success,
            "enable slot failed! {:#?}",
            request_result
        );

        request_result.slot_id()
    }

    async fn disable_slot(&mut self, _slot: u8) -> Result<RequestResult, u8> {
        todo!()
    }

    async fn control_transfer(&self, slot: u8, urb_req: ControlTransfer) -> usize {
        let direction = urb_req.request_type.direction;
        let buffer = urb_req.data;

        let mut len = 0;
        let data = if let Some((addr, length)) = buffer {
            let mut data = transfer::DataStage::default();
            len = length;
            data.set_data_buffer_pointer(addr as u64)
                .set_trb_transfer_length(len as _)
                .set_direction(direction.into());
            Some(data)
        } else {
            None
        };

        let setup = *transfer::SetupStage::default()
            .set_request_type(urb_req.request_type.into())
            .set_request(urb_req.request.into())
            .set_value(urb_req.value)
            .set_index(urb_req.index)
            .set_transfer_type({
                if buffer.is_some() {
                    match direction {
                        Direction::In => TransferType::In,
                        Direction::Out => TransferType::Out,
                    }
                } else {
                    TransferType::No
                }
            })
            .set_length(len as u16);
        trace!("{:#?}", setup);

        let mut status = *transfer::StatusStage::default().set_interrupt_on_completion();

        if urb_req.response {
            status.set_direction();
        }

        //=====post!=======
        let mut trbs: Vec<transfer::Allowed> = Vec::new();

        trbs.push(setup.into());
        if let Some(data) = data {
            trbs.push(data.into());
        }
        trbs.push(status.into());

        let trb_pointers: Vec<usize> = {
            let mut writer = self.dev_ctx.write().await;
            let ring = writer
                .write_transfer_ring(slot, 0)
                .expect("initialization on transfer rings got some issue, fixit.");
            trbs.into_iter()
                .map(|trb| ring.enque_transfer(trb))
                .collect()
        };

        if trb_pointers.len() == 2 {
            trace!(
                "[Transfer] >> setup@{:#X}, status@{:#X}",
                trb_pointers[0],
                trb_pointers[1]
            );
        } else {
            trace!(
                "[Transfer] >> setup@{:#X}, data@{:#X}, status@{:#X}",
                trb_pointers[0],
                trb_pointers[1],
                trb_pointers[2]
            );
        }

        fence(Ordering::Release);
        self.ring_db(slot, None, 1.into());

        trb_pointers.last().unwrap().to_owned()
    }
}

impl<'a, O> Controller<'a, O> for XHCIController<'a, O>
where
    O: PlatformAbstractions,
    [(); O::RING_BUFFER_SIZE]:,
{
    fn new(config: Arc<USBSystemConfig<O>>) -> Self
    where
        Self: Sized,
    {
        let mmio_base = config.base_addr.clone().into();
        unsafe {
            let regs = RegistersBase::new(mmio_base, MemMapper);
            let ext_list = RegistersExtList::new(
                mmio_base,
                regs.capability.hccparams1.read_volatile(),
                MemMapper,
            );

            let hcsp1 = regs.capability.hcsparams1.read_volatile();
            let max_slots = hcsp1.number_of_device_slots();
            let max_ports = hcsp1.number_of_ports();
            let max_irqs = hcsp1.number_of_interrupts();
            let page_size = regs.operational.pagesize.read_volatile().get();
            debug!(
                "{TAG} Max_slots: {}, max_ports: {}, max_irqs: {}, page size: {}",
                max_slots, max_ports, max_irqs, page_size
            );

            trace!("new dev ctx!");
            let dev_ctx = DeviceContextList::new(config.clone());

            // Create the command ring with 4096 / 16 (TRB size) entries, so that it uses all of the
            // DMA allocation (which is at least a 4k page).
            let entries_per_page = O::PAGE_SIZE / mem::size_of::<ring::TrbData>();
            trace!("new cmd ring");
            let cmd = Ring::new(config.os.clone(), entries_per_page, true);
            trace!("new evt ring");
            let event = EventRing::new(config.os.clone());
            debug!("{TAG} ring size {}", cmd.len());

            Self {
                regs: regs.into(),
                ext_list,
                config: config.clone(),
                max_slots,
                max_ports,
                max_irqs,
                scratchpad_buf_arr: OnceCell::new(),
                cmd: cmd.into(),
                event,
                dev_ctx: dev_ctx.into(),
                devices: Vec::new().into(),
                finish_jobs: BTreeMap::new().into(),
                requests: Vec::new().into(), //safety: only controller itself could fetch, all acccess via run_once
                extra_works: BTreeMap::new(),
                event_tables: EventHandlerTable::new().into(),
            }
        }
    }

    fn init(&self) {
        block_on(
            //safety: no need for reschedule, set() on Oncecell should complete instantly
            self.chip_hardware_reset()
                .set_max_device_slots()
                .set_dcbaap()
                .set_cmd_ring()
                .init_ir()
                .setup_scratchpads(),
        )
        .start()
        .initial_probe();
    }

    fn device_accesses(&self) -> &Vec<Arc<USBDevice<'a, O>>> {
        unsafe { self.devices.get().as_ref_unchecked() }
    }

    fn register_event_handler(&self, handler: super::controller_events::EventHandler<'a, O>) {
        while let Some(mut writer) = self.event_tables.try_write() {
            writer.register(handler);
            return;
        }
    }
}

fn parse_default_max_packet_size_from_speed(port_speed: u8) -> u16 {
    match port_speed {
        1 | 3 => 64,
        2 => 8,
        4 => 512,
        v => unimplemented!("PSI: {}", v),
    }
}
