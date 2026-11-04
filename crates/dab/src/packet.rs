//! Packet mode data (EN 300 401 clause 5.3.2): packets of a sub-channel are
//! reassembled into MSC data groups per packet address.

use crate::crc::check_crc16;
use std::collections::HashMap;

#[derive(Default)]
pub struct PacketDecoder {
    groups: HashMap<u16, Vec<u8>>,
    pub packets: u64,
    pub crc_errors: u64,
}

impl PacketDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one logical frame of the sub-channel; complete data groups are
    /// passed to `out` together with their packet address.
    pub fn push(&mut self, frame: &[u8], mut out: impl FnMut(u16, Vec<u8>)) {
        let mut i = 0;
        while i + 3 <= frame.len() {
            let len = match frame[i] >> 6 {
                0 => 24,
                1 => 48,
                2 => 72,
                _ => 96,
            };
            if i + len > frame.len() {
                break;
            }
            let pkt = &frame[i..i + len];
            i += len;
            self.packets += 1;
            if !check_crc16(pkt) {
                self.crc_errors += 1;
                continue;
            }
            let first_last = (pkt[0] >> 2) & 0x03;
            let address = (((pkt[0] & 0x03) as u16) << 8) | pkt[1] as u16;
            let command = pkt[2] & 0x80 != 0;
            let useful = (pkt[2] & 0x7F) as usize;
            if address == 0 || command || 3 + useful > len - 2 {
                // padding packets use address 0, FEC packets 1022 carry no data groups
                continue;
            }
            let data = &pkt[3..3 + useful];
            match first_last {
                0b11 => out(address, data.to_vec()),
                0b10 => {
                    self.groups.insert(address, data.to_vec());
                }
                0b00 => {
                    if let Some(g) = self.groups.get_mut(&address) {
                        g.extend_from_slice(data);
                        if g.len() > 65536 {
                            self.groups.remove(&address);
                        }
                    }
                }
                _ => {
                    if let Some(mut g) = self.groups.remove(&address) {
                        g.extend_from_slice(data);
                        out(address, g);
                    }
                }
            }
        }
    }
}
