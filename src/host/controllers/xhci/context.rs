use core::cell::SyncUnsafeCell;
use core::usize;

use crate::abstractions::dma::DMA;
use crate::abstractions::{PlatformAbstractions, SystemWordWide, USBSystemConfig};

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::{format, vec};
use core::ops::{Deref, DerefMut};
use log::trace;
use xhci::context::{Device, Device32Byte, DeviceHandler, Input64Byte, InputHandler};
use xhci::context::{Device64Byte, Input32Byte};
use xhci::ring::trb::transfer;

use super::ring::Ring;
const NUM_EPS: usize = 32;

pub struct DeviceContextList<O, const RING_BUFFER_SIZE: usize>
where
    O: PlatformAbstractions,
{
    config: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>,
    pub dcbaa: SyncUnsafeCell<DMA<[u64; 256], O>>,
    pub device_ctx_inners: BTreeMap<u8, DeviceCtxInner<O>>,
}

pub struct DeviceCtxInner<O>
where
    O: PlatformAbstractions,
{
    pub out_ctx: DeviceCtx<O>,
    pub in_ctx: InputCtx<O>,
    pub transfer_rings: Vec<Ring<O>>,
}

pub enum InputCtx<O>
where
    O: PlatformAbstractions,
{
    B64(DMA<Input64Byte, O>),
    B32(DMA<Input32Byte, O>),
}
pub enum DeviceCtx<O>
where
    O: PlatformAbstractions,
{
    B64(DMA<Device64Byte, O>),
    B32(DMA<Device32Byte, O>),
}

impl<O> DeviceCtx<O>
where
    O: PlatformAbstractions,
{
    pub fn new(ctx_size: SystemWordWide, a: O::DMA) -> Self {
        match ctx_size {
            SystemWordWide::X64 => {
                Self::B64(DMA::new(Device64Byte::new_64byte(), 4096, a).fill_zero())
            }
            SystemWordWide::X32 => {
                Self::B32(DMA::new(Device32Byte::new_32byte(), 4096, a).fill_zero())
            }
        }
    }

    pub fn access_mut(&mut self) -> &mut dyn DeviceHandler {
        match self {
            DeviceCtx::B64(dma) => dma.deref_mut(),
            DeviceCtx::B32(dma) => dma.deref_mut(),
        }
    }

    pub fn access(&self) -> &dyn DeviceHandler {
        match self {
            DeviceCtx::B64(dma) => dma.deref(),
            DeviceCtx::B32(dma) => dma.deref(),
        }
    }

    pub fn addr(&self) -> O::VirtAddr {
        match self {
            DeviceCtx::B64(dma) => dma.addr(),
            DeviceCtx::B32(dma) => dma.addr(),
        }
    }
}

impl<O> InputCtx<O>
where
    O: PlatformAbstractions,
{
    pub fn new(ctx_size: SystemWordWide, a: O::DMA) -> Self {
        match ctx_size {
            SystemWordWide::X64 => {
                Self::B64(DMA::new(Input64Byte::new_64byte(), 4096, a.clone()).fill_zero())
            }
            SystemWordWide::X32 => {
                Self::B32(DMA::new(Input32Byte::new_32byte(), 4096, a.clone()).fill_zero())
            }
        }
    }

    pub fn access(&mut self) -> &mut dyn InputHandler {
        match self {
            InputCtx::B64(dma) => &mut **dma,
            InputCtx::B32(dma) => &mut **dma,
        }
    }

    pub fn addr(&self) -> O::VirtAddr {
        match self {
            InputCtx::B64(dma) => dma.addr(),
            InputCtx::B32(dma) => dma.addr(),
        }
    }

    pub fn copy_from_output(&mut self, output: &DeviceCtx<O>) {
        match (self, output) {
            (InputCtx::B64(i), DeviceCtx::B64(o)) => (&mut **i).copy_from_output(&**o),
            (InputCtx::B32(i), DeviceCtx::B32(o)) => (&mut **i).copy_from_output(&**o),
            _ => panic!("it wont happen"),
        }
    }
}

impl<O, const RING_BUFFER_SIZE: usize> DeviceContextList<O, RING_BUFFER_SIZE>
where
    O: PlatformAbstractions,
{
    pub fn new(cfg: Arc<USBSystemConfig<O, RING_BUFFER_SIZE>>) -> Self {
        Self {
            config: cfg.clone(),
            dcbaa: DMA::new([0u64; 256], 4096, cfg.os.dma_alloc()).into(),
            device_ctx_inners: BTreeMap::new(),
        }
    }

    pub fn dcbaap(&self) -> O::VirtAddr {
        (unsafe { self.dcbaa.get().read_volatile() }.as_ptr() as usize).into()
    }

    pub fn write_transfer_ring(&mut self, slot: u8, dci: usize) -> Option<&mut Ring<O>> {
        self.device_ctx_inners
            .get_mut(&(slot as _))
            .expect(format!("no such transfer ring at slot {slot}").as_str())
            .transfer_rings
            .get_mut(dci - 1)
    }

    pub fn read_transfer_ring(&self, slot: u8, dci: usize) -> Option<&Ring<O>> {
        assert!(dci > 0 && dci < 32);
        self.device_ctx_inners
            .get(&(slot as _))?
            .transfer_rings
            .get(dci - 1)
    }

    pub fn new_slot(
        &mut self,
        slot: u8,
        num_ep: usize, // cannot lesser than 0, and consider about alignment, use usize
    ) {
        let os = &self.config.os;

        let out_ctx = DeviceCtx::new(O::WORD, os.dma_alloc());
        let in_ctx = InputCtx::new(O::WORD, os.dma_alloc());
        let dcbaap = out_ctx.addr();

        trace!("inserted new transfer ring at slot {}", slot);

        self.device_ctx_inners.insert(
            slot,
            DeviceCtxInner {
                in_ctx,
                out_ctx,
                transfer_rings: {
                    (0..num_ep)
                        .map(|i| (i, Ring::new(os.clone(), 32, true)))
                        .map(|(i, r)| {
                            // if i == 0 {
                            // r
                            // } else {
                            Self::prepare_transfer_ring(r)
                            // }
                        }) //only for non control endpoint
                        .collect()
                },
            },
        );

        let get_mut = self.dcbaa.get_mut();
        get_mut[slot as usize] = O::PhysAddr::from(dcbaap).into() as _;
    }

    fn prepare_transfer_ring(mut r: Ring<O>) -> Ring<O> {
        //in our code, the init state of transfer ring always has ccs = 0, so we use ccs =1 to fill transfer ring
        let mut norm = transfer::Normal::default();
        norm.set_cycle_bit();
        r.enque_trbs_no_check(vec![norm.into_raw(); r.len() - 1]); //the n'th is link trb
        r
    }
}

use tock_registers::interfaces::Writeable;
use tock_registers::register_structs;
use tock_registers::registers::ReadWrite;

register_structs! {
    pub ScratchpadBufferEntry{
        (0x000 => value_low: ReadWrite<u32>),
        (0x004 => value_high: ReadWrite<u32>),
        (0x008 => @END),
    }
}

impl ScratchpadBufferEntry {
    pub fn set_addr(&mut self, addr: u64) {
        self.value_low.set(addr as u32);
        self.value_high.set((addr >> 32) as u32);
    }
}

pub struct ScratchpadBufferArray<O>
where
    O: PlatformAbstractions,
{
    pub entries: DMA<[ScratchpadBufferEntry], O>,
    pub pages: Vec<DMA<[u8], O>>,
}

unsafe impl<O: PlatformAbstractions> Sync for ScratchpadBufferArray<O> {}

impl<O> ScratchpadBufferArray<O>
where
    O: PlatformAbstractions,
{
    pub fn new(entries: u32, os: O) -> Self {
        let page_size = O::PAGE_SIZE;
        let align = 64;

        let mut entries: DMA<[ScratchpadBufferEntry], O> =
            DMA::zeroed(entries as usize, align, os.dma_alloc());

        let pages = entries
            .iter_mut()
            .map(|entry| {
                let dma = DMA::zeroed(page_size, align, os.dma_alloc());
                let paddr = O::PhysAddr::from(dma.addr()).into();

                assert_eq!(paddr % page_size, 0);
                entry.set_addr(paddr as _);
                dma
            })
            .collect();

        Self { entries, pages }
    }
    pub fn register(&self) -> O::VirtAddr {
        self.entries.addr()
    }
}
