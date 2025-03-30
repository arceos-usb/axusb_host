use alloc::vec::Vec;
use log::trace;
use xhci::ring::trb::{command, transfer, Link};

use crate::abstractions::{dma::DMA, PlatformAbstractions};

const TRB_LEN: usize = 4;
pub type TrbData = [u32; TRB_LEN];

pub struct Ring<O: PlatformAbstractions> {
    link: bool,
    pub trbs: DMA<[TrbData], O>,
    pub i: usize,
    pub cycle: bool,
}

impl<O: PlatformAbstractions> Ring<O> {
    pub fn new(os: O, len: usize, link: bool) -> Self {
        let a = os.dma_alloc();
        let trbs = DMA::new_vec([0; TRB_LEN], len, 64, a);
        Self {
            trbs,
            i: 0,
            cycle: link,
            link,
        }
    }
    pub fn len(&self) -> usize {
        self.trbs.len()
    }

    fn get_trb(&self) -> &TrbData {
        &self.trbs[self.i]
    }

    pub fn register(&self) -> O::VirtAddr {
        (self.get_trb().as_ptr() as usize).into()
    }

    pub fn enque_command(&mut self, mut trb: command::Allowed) -> O::PhysAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }
        let addr = self.enque_trb(trb.into_raw());
        trace!("[CMD] >> {:?} @{:X}", trb, addr.clone().into());
        O::PhysAddr::from(addr)
    }

    pub fn enque_transfer(&mut self, mut trb: transfer::Allowed) -> O::PhysAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }

        O::PhysAddr::from(self.enque_trb(trb.into_raw()))
    }

    fn enque_trb(&mut self, trb: TrbData) -> O::VirtAddr {
        self.trbs[self.i].copy_from_slice(&trb);
        let addr = self.trbs[self.i].as_ptr() as usize;

        #[cfg(feature = "trace_xhci_enque_trb")]
        trace!(
                "enqueued {} @{:#X}\n{:x}\n{:x}\n{:x}\n{:x}\n------------------------------------------------",
                self.i, addr, trb[0], trb[1], trb[2], trb[3]
            );

        self.next_index();
        addr.into()
    }

    pub fn enque_trbs_no_check(&mut self, trb: Vec<TrbData>) {
        for ele in trb {
            self.trbs[self.i].copy_from_slice(&ele);

            self.next_index();
        }
    }

    fn next_index(&mut self) -> usize {
        self.i += 1;
        let mut need_link = false;
        let len = self.len();

        // link模式下，最后一个是Link
        if self.link && self.i >= len - 1 {
            self.i = 0;
            need_link = true;
        } else if self.i >= len {
            self.i = 0;
        }

        if need_link {
            {
                trace!(
                    "link! current cycle: {}, after link:{}",
                    self.cycle,
                    !self.cycle
                );
            }
            let address = O::VirtAddr::from(self.trbs[0].as_ptr() as usize);
            let mut link = Link::new();
            link.set_ring_segment_pointer(O::PhysAddr::from(address).into() as u64)
                .set_toggle_cycle();

            if self.cycle {
                link.set_cycle_bit();
            } else {
                link.clear_cycle_bit();
            }
            let trb = command::Allowed::Link(link);
            let link_trb = trb.into_raw();
            let this_trb = &mut self.trbs[len - 1];
            this_trb.copy_from_slice(&link_trb);

            self.cycle = !self.cycle;
        }

        self.i
    }

    /// 完成一次循环返回true
    pub fn inc_deque(&mut self) -> bool {
        self.i += 1;
        let mut is_cycle = false;
        let len = self.len();
        if self.link {
        } else if self.i >= len {
            self.i = 0;
            self.cycle = !self.cycle;
            is_cycle = true;
        }

        is_cycle
    }

    pub fn current_data(&self) -> (&TrbData, bool) {
        (&self.trbs[self.i], self.cycle)
    }

    pub fn get_len(&self) -> usize {
        self.trbs.len()
    }
}
