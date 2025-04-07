use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_lock::RwLock;
use log::{info, warn};
use usb_descriptor_decoder::descriptors::{
    desc_device::StandardUSBDeviceClassCode,
    desc_endpoint::Endpoint,
    desc_hid::{Hid, USBHIDProtocolDescriptorType, USBHIDSubclassDescriptorType},
    desc_interface::TopologyUSBFunction,
};

use crate::{
    abstractions::PlatformAbstractions,
    driver::driverapi::{USBSystemDriverModule, USBSystemDriverModuleInstanceFunctionalInterface},
    host::device::USBDevice,
    usb::operations::RequestResult,
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
        device: &alloc::sync::Arc<crate::host::device::USBDevice<O, RING_BUFFER_SIZE>>,
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
        let map = device
            .descriptor
            .get()
            .unwrap()
            .configs
            .iter()
            .find(|config| config.desc.config_val() == device.current_config)
            .unwrap()
            .functions
            .iter()
            .find_map(|function| match function {
                TopologyUSBFunction::Interface(usbinterfaces) => {
                    usbinterfaces.iter().find(|intf| {
                        intf.interface.interface_class == StandardUSBDeviceClassCode::HID as u8
                            && intf.interface.interface_protocol
                                == USBHIDProtocolDescriptorType::Mouse as u8
                    })
                }
                _ => None,
            })
            .map(|intf| HIDMouseModuleInstance {
                device_ref: device.clone(),
                interface: intf.interface.interface,
                alt_interface: intf.interface.alternate_setting,
            })
            .unwrap();
        return Some(Arc::new(RwLock::new(map)));
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
    interface: u8,
    alt_interface: u8,
}

#[async_trait::async_trait]
impl<'a, O, const RING_BUFFER_SIZE: usize> USBSystemDriverModuleInstanceFunctionalInterface<'a, O>
    for HIDMouseModuleInstance<O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
{
    async fn run(&mut self) {
        let (i, o, e) = {
            let find_map = self
                .device_ref
                .descriptor
                .get()
                .unwrap()
                .configs
                .iter()
                .find_map(|cfg| {
                    cfg.functions.iter().find_map(|func| match func {
                        TopologyUSBFunction::InterfaceAssociation(_, _, _) => None,
                        TopologyUSBFunction::Interface(interfaces) => interfaces.iter().find(|i| {
                            i.interface.interface_number == self.interface
                                && i.interface.alternate_setting == self.alt_interface
                        }),
                    })
                });

            if let None = find_map {
                warn!(
                    "not find interface {}:{}",
                    self.interface, self.alt_interface
                );
                return;
            }
            find_map.map(|i| (i, &i.extra, &i.endpoints)).unwrap()
        };

        let configure_semaphore = self.device_ref.async_acquire_cfg_sem().await;

        match self
            .device_ref
            .request_once(crate::usb::operations::RequestedOperation::EnableEndpoints(
                e.clone(),
            ))
            .await
        {
            Ok(o) if o != RequestResult::Success => {
                warn!(
                    "err on request enable endpoint, so just die...  error code {:?}",
                    o
                );
                return ();
            }
            Err(e) => {
                warn!("err on request enable endpoint, so just die...  error code {e}");
                return ();
            }
            success => {}
        }
    }
}
