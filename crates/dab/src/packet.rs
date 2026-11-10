//! Packet mode data (EN 300 401 clause 5.3.2): packets of a sub-channel are
//! reassembled into MSC data groups per packet address.

use crate::crc::check_crc16;
use std::collections::HashMap;

#[derive(Default)]
pub struct PacketDecoder {
    groups: HashMap<u16, Vec<u8>>,
    pub packets: u64,
    pub crc_errors: u64,
    /// Per address: the continuity index of its last packet and the CRC
    /// errors counted then; jumps of the index with no CRC error between,
    /// and those of them right before a MOT directory (mot.rs: ciLoop).
    last_ci: HashMap<u16, (u8, u64)>,
    pub ci_jumps: HashMap<u16, (u32, u32)>,
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
            let ci = (pkt[0] >> 4) & 0x03;
            if let Some((last, errors)) = self.last_ci.insert(address, (ci, self.crc_errors)) {
                if ci != (last + 1) & 3 && errors == self.crc_errors {
                    let directory = first_last & 0b10 != 0 && data.first().is_some_and(|b| b & 0x0F == 6);
                    let j = self.ci_jumps.entry(address).or_default();
                    j.0 += 1;
                    j.1 += directory as u32;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16_ccitt;

    /// A 24-byte packet: continuity index, first/last flags, address, data.
    fn packet(ci: u8, first_last: u8, data: &[u8]) -> Vec<u8> {
        let mut p = vec![(ci << 4) | (first_last << 2), 1, data.len() as u8];
        p.extend_from_slice(data);
        p.resize(22, 0);
        let crc = crc16_ccitt(&p);
        p.extend_from_slice(&crc.to_be_bytes());
        p
    }

    #[test]
    fn continuity_jumps_before_a_directory_are_counted() {
        let mut d = PacketDecoder::new();
        // a carousel of 3 packets looped: the index jumps where it starts again
        // (its first data group a MOT directory, type 6)
        let carousel = [packet(0, 0b11, &[0x36, 0]), packet(1, 0b11, &[0x34, 0]), packet(2, 0b11, &[0x34, 0])];
        for _ in 0..4 {
            for p in &carousel {
                d.push(p, |_, _| {});
            }
        }
        assert_eq!(d.ci_jumps.get(&1), Some(&(3, 3)));
        // a packet lost to a reception error is not the carousel's fault
        let mut d = PacketDecoder::new();
        let mut bad = packet(1, 0b11, &[0x34, 0]);
        bad[5] ^= 0xFF;
        for p in [&carousel[0], &bad, &carousel[2]] {
            d.push(p, |_, _| {});
        }
        assert_eq!(d.ci_jumps.get(&1), None);
    }
}
