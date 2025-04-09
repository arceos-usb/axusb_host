use core::{
    future::{Future, IntoFuture},
    pin::Pin,
};

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_lock::{OnceCell, RwLock};
use embassy_futures::yield_now;
use futures::{task::AtomicWaker, FutureExt};
use log::{info, trace, warn};
use num_traits::Zero;
use squeak::Response;
use usb_descriptor_decoder::descriptors::{
    desc_device::StandardUSBDeviceClassCode,
    desc_endpoint::{Endpoint, EndpointType},
    desc_hid::{
        HIDDescriptorTypes, Hid, USBHIDProtocolDescriptorType, USBHIDSubclassDescriptorType,
    },
    desc_interface::{TopologyUSBFunction, USBInterface},
};

use crate::{
    abstractions::{dma::DMA, PlatformAbstractions},
    driver::driverapi::{USBSystemDriverModule, USBSystemDriverModuleInstanceFunctionalInterface},
    host::device::USBDevice,
    usb::operations::{
        control::{
            bRequest, bRequestStandard, bmRequestType, construct_control_transfer_type,
            ControlTransfer, DataTransferType, Recipient,
        },
        interrupt::InterruptTransfer,
        Direction, RequestResult, RequestedOperation,
    },
};

pub struct HIDMouseModule;

impl<'a, O, const RING_BUFFER_SIZE: usize> USBSystemDriverModule<'a, O, RING_BUFFER_SIZE>
    for HIDMouseModule
where
    'a: 'static,
    O: PlatformAbstractions + 'a,
{
    fn should_active(
        &self,
        device: alloc::sync::Arc<crate::host::device::USBDevice<O, RING_BUFFER_SIZE>>,
        config: &alloc::sync::Arc<crate::abstractions::USBSystemConfig<O, RING_BUFFER_SIZE>>,
    ) -> Option<
        alloc::sync::Arc<
            async_lock::RwLock<
                dyn crate::driver::driverapi::USBSystemDriverModuleInstanceFunctionalInterface<
                    'a,
                    O,
                >,
            >,
        >,
    > {
        trace!("testing device if it's hid mouse...");
        let usbdevice = device.clone();

        device
            .descriptor
            .get()
            .unwrap()
            .configs
            .iter()
            .find(|config| config.desc.config_val() == device.current_config)
            .unwrap()
            .functions
            .iter()
            .find_map(|function| match function.as_ref() {
                TopologyUSBFunction::Interface(usbinterfaces) => usbinterfaces
                    .iter()
                    .find(|intf| {
                        intf.interface.interface_class == StandardUSBDeviceClassCode::HID as u8
                            && intf.interface.interface_protocol
                                == USBHIDProtocolDescriptorType::Mouse as u8
                    })
                    .and_then(move |a| Some((a.clone(), usbinterfaces.clone()))),
                _ => None,
            })
            .map(
                move |(intf, alts)| -> Arc<
                    RwLock<dyn USBSystemDriverModuleInstanceFunctionalInterface<'a, O>>,
                > {
                    trace!("yes it is!");
                    Arc::new(RwLock::new(HIDMouseModuleInstance {
                        device_ref: usbdevice,
                        interface_refs: alts,
                        selected_alt: intf,
                        hid_report_decoder: OnceCell::new(),
                    }))
                },
            )
    }

    //
    //     let (class, _, protocol) = function;
    // if class == StandardUSBDeviceClassCode::HID as u8
    //     && protocol == USBHIDProtocolDescriptorType::Mouse as u8
    //     && let USBFunctionExpressions::Interface(interface) = function
    // {
    //     Some(Arc::new(RwLock::new(HIDMouseModuleInstance {
    //         device_ref: device.clone(),
    //         interface: interface.interface_number,
    //         alt_interface: interface.alternate_setting,
    //     })));
    // }
    // None
    //

    fn preload_module(&self) {
        info!("loaded simple hid mouse driver!")
    }

    fn name(&self) -> &'a str {
        "hid_mouse"
    }
}

pub struct HIDMouseModuleInstance<O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions,
{
    device_ref: Arc<USBDevice<O, RING_BUFFER_SIZE>>,
    interface_refs: Vec<Arc<USBInterface>>,
    selected_alt: Arc<USBInterface>,
    hid_report_decoder: OnceCell<axhid::report_handler::ReportHandler>,
}

impl<'a, O, const RING_BUFFER_SIZE: usize> USBSystemDriverModuleInstanceFunctionalInterface<'a, O>
    for HIDMouseModuleInstance<O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
    'a: 'static,
{
    fn run(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>> {
        Box::pin(self.work_fut().into_future())
    }

    fn pre_drop(&'a self) {
        todo!()
    }
}
impl<'a, O, const RING_BUFFER_SIZE: usize> HIDMouseModuleInstance<O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
{
    pub async fn work_fut(&mut self) {
        trace!("hid mouse driver instance running...");

        self.device_ref
            .enable_function(self.selected_alt.clone())
            .await;

        self.device_ref
            .request_once(crate::usb::operations::RequestedOperation::Control(
                ControlTransfer {
                    request_type: bmRequestType::new(
                        Direction::Out,
                        DataTransferType::Standard,
                        Recipient::Device,
                    ),
                    request: bRequest::Standard(bRequestStandard::SetConfiguration),
                    index: self.selected_alt.interface.interface_number as _,
                    value: self.device_ref.current_config as _,
                    data: None,
                    response: true,
                },
            ))
            .await
            .unwrap();

        self.device_ref
            .request_once(crate::usb::operations::RequestedOperation::Control(
                ControlTransfer {
                    request_type: bmRequestType::new(
                        Direction::Out,
                        DataTransferType::Standard,
                        Recipient::Interface,
                    ),
                    request: bRequest::Standard(bRequestStandard::SetInterface),
                    index: self.selected_alt.interface.alternate_setting as u16,
                    value: self.selected_alt.interface.interface_number as u16,
                    data: None,
                    response: true,
                },
            ))
            .await
            .unwrap();
        trace!("request success!, now we could request actual data!");
        {
            let hid_report: DMA<[u8], O> = DMA::new_vec(
                0u8,
                O::PAGE_SIZE,
                O::PAGE_SIZE,
                self.device_ref.config.os.dma_alloc(),
            );

            self.device_ref
                .request_once(crate::usb::operations::RequestedOperation::Control(
                    ControlTransfer {
                        request_type: bmRequestType::new(
                            Direction::In,
                            DataTransferType::Standard,
                            Recipient::Interface,
                        ),
                        request: bRequest::Standard(bRequestStandard::GetDescriptor),
                        index: self.selected_alt.interface.alternate_setting as u16,
                        value: construct_control_transfer_type(
                            HIDDescriptorTypes::HIDReport as u8,
                            0,
                        )
                        .bits(),
                        data: Some(hid_report.phys_addr_len_tuple().into()),
                        response: true,
                    },
                ))
                .await
                .unwrap();

            let (last_offset, _) = hid_report
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != 0)
                .last()
                .unwrap();

            let report_handler =
                axhid::report_handler::ReportHandler::new(&hid_report[0..=last_offset]).unwrap();
            drop(hid_report);

            let _ = self.hid_report_decoder.set(report_handler).await;
        }

        let ep_id = self
            .selected_alt
            .endpoints
            .iter()
            .find(|ep| ep.endpoint_type() == EndpointType::InterruptIn)
            .unwrap()
            .doorbell_value_aka_dci() as _;
        let aligned_size = self
            .hid_report_decoder
            .get()
            .map(|handler| handler.total_byte_length)
            .unwrap()
            .next_power_of_two();

        let mut hid_response: DMA<[u8], O> = DMA::new_vec(
            0u8,
            aligned_size,
            aligned_size,
            self.device_ref.config.os.dma_alloc(),
        );
        trace!("prepare complete!");
        loop {
            let request_result = self
                .device_ref
                .request_once(RequestedOperation::Interrupt(InterruptTransfer {
                    endpoint_id: ep_id,
                    buffer_addr_len: hid_response.phys_addr_len_tuple().into(),
                    short_packet_ok: true,
                }))
                .await;

            if let Some(handler) = self.hid_report_decoder.get_mut() {
                let _ = handler
                    .handle(&hid_response)
                    .inspect(|ok| info!("response! {:#?}", ok));
            }
        }

        // self.device_ref
        //     .keep_no_response(
        //         crate::usb::operations::RequestedOperation::Interrupt(InterruptTransfer {
        //             endpoint_id: ep_id,
        //             buffer_addr_len: hid_response.phys_addr_len_tuple().into(),
        //             short_packet_ok: true,
        //         }),
        //         ep_id as _,
        //     )
        //     .await;

        // loop {
        //     if hid_response.iter().any(|a| !a.is_zero())
        //         && let Some(handler) = self.hid_report_decoder.get_mut()
        //     {
        //         let _ = handler
        //             .handle(&hid_response)
        //             .inspect(|ok| info!("response! {:#?}", ok));
        //     }
        //     yield_now().await
        // }
    }
}
