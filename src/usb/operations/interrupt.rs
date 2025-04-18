#[derive(Debug, Clone)]
pub struct InterruptTransfer {
    pub endpoint_id: usize,
    pub buffer_addr_len: (usize, usize),
    pub short_packet_ok: bool,
}
