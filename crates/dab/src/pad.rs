//! Programme Associated Data: F-PAD/X-PAD parsing and reassembly of X-PAD
//! data groups (EN 300 401 clause 7.4).

use crate::crc::check_crc16;

/// A complete data group carried in X-PAD.
#[derive(Debug)]
pub enum PadEvent {
    /// Dynamic label data group (prefix, characters, CRC verified).
    Dls(Vec<u8>),
    /// MSC data group of an X-PAD user application (identified by its start AppTy).
    DataGroup { app_type: u8, data: Vec<u8> },
}

const XPAD_LEN: [usize; 8] = [4, 6, 8, 12, 16, 24, 32, 48];

#[derive(Clone, Copy)]
struct Ci {
    app_type: u8,
    len: usize,
}

#[derive(Default)]
struct Assembler {
    buf: Vec<u8>,
    expected: usize,
    active: bool,
}

#[derive(Default)]
pub struct PadDecoder {
    last_ci: Option<Ci>,
    dgli_len: usize,
    dgli: Assembler,
    dls: Assembler,
    apps: Vec<(u8, Assembler)>,
    xpad: Vec<u8>,
    pub errors: u64,
}

impl PadDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        let apps: Vec<u8> = self.apps.iter().map(|(a, _)| *a).collect();
        *self = Self::default();
        for a in apps {
            self.register_app(a);
        }
    }

    /// Register an X-PAD application type (start type, continuation is type + 1)
    /// carrying MSC data groups, e.g. 12 for MOT slideshow.
    pub fn register_app(&mut self, app_type: u8) {
        if !self.apps.iter().any(|(a, _)| *a == app_type) {
            self.apps.push((app_type, Assembler::default()));
        }
    }

    /// Process the PAD of one audio frame. `xpad_rev` holds the X-PAD bytes in
    /// transmission (reversed) order, `fpad` the two F-PAD bytes.
    pub fn process(&mut self, xpad_rev: &[u8], exact: bool, fpad: [u8; 2], out: &mut Vec<PadEvent>) {
        self.xpad.clear();
        self.xpad.extend(xpad_rev.iter().rev());
        let xpad_len = self.xpad.len();
        let fpad_type = fpad[0] >> 6;
        let xpad_ind = (fpad[0] >> 4) & 0x03;
        let ci_flag = fpad[1] & 0x02 != 0;
        let prev = self.last_ci.take();
        if fpad_type != 0 || xpad_ind == 0 {
            return;
        }
        let mut cis: Vec<Ci> = Vec::with_capacity(4);
        let mut ci_len = 0usize;
        if ci_flag {
            match xpad_ind {
                1 => {
                    if xpad_len < 1 {
                        return;
                    }
                    let t = self.xpad[0] & 0x1F;
                    ci_len = 1;
                    if t != 0 {
                        cis.push(Ci { app_type: t, len: 3 });
                    }
                }
                2 => {
                    for i in 0..4 {
                        if i >= xpad_len {
                            return;
                        }
                        let raw = self.xpad[i];
                        ci_len += 1;
                        if raw & 0x1F == 0 {
                            break;
                        }
                        cis.push(Ci { app_type: raw & 0x1F, len: XPAD_LEN[(raw >> 5) as usize] });
                    }
                }
                _ => return,
            }
        } else if let Some(p) = prev {
            // No CI list: the field continues the application of the last
            // subfield of the previous field. With DAB+ the X-PAD length is
            // known exactly; a short X-PAD field holds 4 bytes then (3 after a
            // CI); otherwise it equals that subfield's length.
            let len = if exact {
                xpad_len
            } else if xpad_ind == 1 {
                4.min(xpad_len)
            } else {
                p.len.min(xpad_len)
            };
            cis.push(Ci { app_type: p.app_type, len });
        }
        if cis.is_empty() {
            return;
        }
        let announced: usize = ci_len + cis.iter().map(|c| c.len).sum::<usize>();
        if announced > xpad_len {
            // the subfields do not fit: corrupted
            self.errors += 1;
            return;
        }
        // Some encoders send longer X-PAD fields than the
        // subfields need: the remaining bytes are padding.
        if std::env::var("PADDEBUG").is_ok() {
            eprintln!(
                "PAD xlen {} ci {} cis {:?} dgli {}",
                xpad_len,
                ci_flag,
                cis.iter().map(|c| (c.app_type, c.len)).collect::<Vec<_>>(),
                self.dgli_len
            );
        }
        let mut off = ci_len;
        let mut cont_type = 0u8;
        let mut last_len = 0usize;
        for ci in cis {
            last_len = ci.len;
            let sub = self.xpad[off..off + ci.len].to_vec();
            match ci.app_type {
                1 => {
                    // data group length indicator: 2 bytes (rfa + 14 bit length) + CRC
                    if ci_flag {
                        self.dgli.buf.clear();
                        self.dgli.active = true;
                    }
                    if self.dgli.active {
                        self.dgli.buf.extend_from_slice(&sub);
                        if self.dgli.buf.len() >= 4 {
                            if check_crc16(&self.dgli.buf[..4]) {
                                self.dgli_len = (((self.dgli.buf[0] & 0x3F) as usize) << 8) | self.dgli.buf[1] as usize;
                            }
                            self.dgli.active = false;
                        }
                    }
                    cont_type = 1;
                }
                2 | 3 => {
                    let start = ci.app_type == 2;
                    if start {
                        self.dls.buf.clear();
                        self.dls.active = true;
                        self.dls.expected = 0;
                    }
                    if self.dls.active {
                        self.dls.buf.extend_from_slice(&sub);
                        if self.dls.expected == 0 && self.dls.buf.len() >= 2 {
                            let p = self.dls.buf[0];
                            let c_flag = p & 0x10 != 0;
                            let field_len = if c_flag {
                                // commands: DL Plus carries a length in the second byte
                                if p & 0x0F == 0x02 {
                                    (self.dls.buf[1] & 0x0F) as usize + 1
                                } else {
                                    0
                                }
                            } else {
                                (p & 0x0F) as usize + 1
                            };
                            self.dls.expected = 2 + field_len + 2;
                        }
                        if self.dls.expected > 0 && self.dls.buf.len() >= self.dls.expected {
                            let dg = self.dls.buf[..self.dls.expected].to_vec();
                            self.dls.active = false;
                            if check_crc16(&dg) {
                                out.push(PadEvent::Dls(dg));
                            } else {
                                self.errors += 1;
                            }
                        }
                    }
                    cont_type = 3;
                }
                t => {
                    let pos = self.apps.iter().position(|(a, _)| t == *a || t == *a + 1);
                    if let Some(p) = pos {
                        let app = self.apps[p].0;
                        let start = t == app;
                        let asm = &mut self.apps[p].1;
                        if start {
                            asm.buf.clear();
                            asm.expected = self.dgli_len;
                            asm.active = asm.expected > 0;
                            self.dgli_len = 0;
                        }
                        if asm.active {
                            asm.buf.extend_from_slice(&sub);
                            if asm.buf.len() >= asm.expected {
                                let dg = asm.buf[..asm.expected].to_vec();
                                asm.active = false;
                                out.push(PadEvent::DataGroup { app_type: app, data: dg });
                            }
                        }
                        cont_type = app + 1;
                    } else {
                        cont_type = 0;
                    }
                }
            }
            off += ci.len;
        }
        if cont_type != 0 {
            self.last_ci = Some(Ci { app_type: cont_type, len: last_len });
        }
    }
}

/// Extract PAD from a DAB+ access unit (data stream element at the start).
/// Returns (reversed X-PAD bytes, F-PAD).
pub fn dabplus_pad(au: &[u8]) -> Option<(&[u8], [u8; 2])> {
    if au.len() < 3 || au[0] >> 5 != 4 {
        return None;
    }
    let mut start = 2usize;
    let mut len = au[1] as usize;
    if len == 255 {
        len += au[2] as usize;
        start += 1;
    }
    if len < 2 || start + len > au.len() {
        return None;
    }
    let pad = &au[start..start + len];
    Some((&pad[..len - 2], [pad[len - 2], pad[len - 1]]))
}

/// Extract PAD from an MP2 frame. `scf_crc_len` is 2 or 4.
pub fn mp2_pad(frame: &[u8], scf_crc_len: usize) -> Option<(&[u8], [u8; 2])> {
    let n = frame.len();
    if n < 4 + scf_crc_len + 2 {
        return None;
    }
    let fpad = [frame[n - 2], frame[n - 1]];
    // everything before ScF-CRC may be X-PAD; limit to a sane size
    let end = n - 2 - scf_crc_len;
    let begin = end.saturating_sub(196).max(4);
    Some((&frame[begin..end], fpad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16_ccitt;

    /// One X-PAD field as MP2 carries it: reversed, in a longer search window.
    fn frame(dec: &mut PadDecoder, logical: &[u8], ci: bool, out: &mut Vec<PadEvent>) {
        let mut window: Vec<u8> = logical.to_vec();
        window.resize(40, 0);
        window.reverse();
        dec.process(&window, false, [0x10, if ci { 0x02 } else { 0x00 }], out);
    }

    #[test]
    fn dls_over_short_xpad_without_known_length() {
        // a whole label "ABCD": prefix, characters, CRC
        let mut dg = vec![0x63, 0x00, b'A', b'B', b'C', b'D'];
        let crc = crc16_ccitt(&dg);
        dg.extend_from_slice(&crc.to_be_bytes());
        let mut dec = PadDecoder::new();
        let mut out = Vec::new();
        // CI (DLS start) + 3 bytes, then 4 bytes per field without CI
        frame(&mut dec, &[0x02, dg[0], dg[1], dg[2]], true, &mut out);
        frame(&mut dec, &dg[3..7], false, &mut out);
        frame(&mut dec, &[dg[7], 0, 0, 0], false, &mut out);
        assert!(matches!(out.as_slice(), [PadEvent::Dls(d)] if *d == dg), "errors {}", dec.errors);
    }
}
