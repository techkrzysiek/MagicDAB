//! ETI (EN 300 799, ETI(NI) frames) and EDI (TS 102 693, AF packets with
//! DETI tags) as produced by a DAB multiplexer such as ODR-DabMux: parsing for
//! the ETI / EDI input and building (test streams).

use crate::crc::{check_crc16, crc16_ccitt};
use crate::puncture::{uep_info, Protection};
use crate::receiver::EtiFrame;

/// Length of an ETI(NI) frame.
pub const ETI_FRAME: usize = 6144;
const FSYNC_EVEN: [u8; 3] = [0x07, 0x3A, 0xB6];
const FSYNC_ODD: [u8; 3] = [0xF8, 0xC5, 0x49];

/// A stream (sub-channel) of a frame to build.
pub struct Stream<'a> {
    pub scid: u8,
    /// Start address (CU).
    pub sad: u16,
    /// Type and protection level as in FIG 0/1 / ETI STC.
    pub tpl: u8,
    pub data: &'a [u8],
}

fn fic_len(mid: u8) -> usize {
    if mid == 3 {
        128
    } else {
        96
    }
}

/// The STC "type and protection level" field of a sub-channel: UEP as 01
/// and the level less one, EEP as 10, the option (A or B) and the level less
/// one (EN 300 799 clause 5.3, as ODR-DabMux writes it).
pub fn tpl(prot: Protection) -> u8 {
    match prot {
        Protection::Uep { index } => 0x10 | uep_info(index).map(|(_, _, level)| level.saturating_sub(1)).unwrap_or(0),
        Protection::Eep { option, level } => 0x20 | ((option & 1) << 2) | (level & 3),
    }
}

/// Parse one ETI(NI) frame (starting with the ERR byte).
pub fn parse_eti(f: &[u8]) -> Option<EtiFrame> {
    if f.len() < 12 || (f[1..4] != FSYNC_EVEN && f[1..4] != FSYNC_ODD) {
        return None;
    }
    let ficf = f[5] & 0x80 != 0;
    let nst = (f[5] & 0x7F) as usize;
    let mid = (f[6] >> 3) & 0x03;
    let fl = ((((f[6] & 0x07) as usize) << 8) | f[7] as usize) * 4;
    if f.len() < 8 + fl {
        return None;
    }
    // EOH: MNSC and the CRC over FC, STC and MNSC
    let eoh = 8 + 4 * nst;
    if f.len() < eoh + 4 || !check_crc16(&f[4..eoh + 4]) {
        return None;
    }
    let stc = &f[8..eoh];
    let mut pos = eoh + 4;
    let fic = if ficf {
        let n = fic_len(mid);
        let d = f.get(pos..pos + n)?.to_vec();
        pos += n;
        d
    } else {
        Vec::new()
    };
    let mut streams = Vec::with_capacity(nst);
    for s in stc.chunks_exact(4) {
        let scid = s[0] >> 2;
        let stl = ((((s[2] & 0x03) as usize) << 8) | s[3] as usize) * 8;
        streams.push((scid, f.get(pos..pos + stl)?.to_vec()));
        pos += stl;
    }
    Some(EtiFrame { fic, streams })
}

/// Build a 6144 byte ETI(NI) frame.
pub fn build_eti(fct: u8, fic: &[u8], streams: &[Stream]) -> Vec<u8> {
    let mut f = Vec::with_capacity(ETI_FRAME);
    f.push(0xFF);
    f.extend_from_slice(if fct % 2 == 0 { &FSYNC_EVEN } else { &FSYNC_ODD });
    let mst_words: usize = fic.len() / 4 + streams.iter().map(|s| s.data.len() / 4).sum::<usize>();
    let fl = streams.len() + 1 + mst_words + 1;
    let fp = (fct % 8) & 0x07;
    f.push(fct);
    f.push(0x80 | streams.len() as u8);
    f.push((fp << 5) | (1 << 3) | ((fl >> 8) as u8 & 0x07));
    f.push(fl as u8);
    for s in streams {
        let stl = s.data.len() / 8;
        f.push((s.scid << 2) | ((s.sad >> 8) as u8 & 0x03));
        f.push(s.sad as u8);
        f.push((s.tpl << 2) | ((stl >> 8) as u8 & 0x03));
        f.push(stl as u8);
    }
    f.extend_from_slice(&[0xFF, 0xFF]); // MNSC
    let crc = crc16_ccitt(&f[4..]);
    f.extend_from_slice(&crc.to_be_bytes());
    let mst = f.len();
    f.extend_from_slice(fic);
    for s in streams {
        f.extend_from_slice(s.data);
    }
    let crc = crc16_ccitt(&f[mst..]);
    f.extend_from_slice(&crc.to_be_bytes());
    f.extend_from_slice(&[0xFF, 0xFF]); // RFU
    f.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // TIST
    f.resize(ETI_FRAME, 0x55);
    f
}

/// Parse an EDI AF packet (header, tag items, CRC) carrying DETI.
pub fn parse_af(p: &[u8]) -> Option<EtiFrame> {
    if p.len() < 10 || &p[..2] != b"AF" || p[9] != b'T' {
        return None;
    }
    let len = u32::from_be_bytes([p[2], p[3], p[4], p[5]]) as usize;
    let crc_present = p[8] & 0x80 != 0;
    let end = 10 + len;
    if p.len() < end + if crc_present { 2 } else { 0 } {
        return None;
    }
    if crc_present && !check_crc16(&p[..end + 2]) {
        return None;
    }
    let mut fic = Vec::new();
    let mut streams: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut deti = false;
    let mut i = 10;
    while i + 8 <= end {
        let name = &p[i..i + 4];
        let bits = u32::from_be_bytes([p[i + 4], p[i + 5], p[i + 6], p[i + 7]]) as usize;
        let n = bits.div_ceil(8);
        let v = p.get(i + 8..i + 8 + n)?;
        i += 8 + n;
        if name == b"deti" {
            if v.len() < 6 {
                return None;
            }
            deti = true;
            let atstf = v[0] & 0x80 != 0;
            let ficf = v[0] & 0x40 != 0;
            let mid = v[3] >> 6;
            let mut pos = 6 + if atstf { 8 } else { 0 };
            if ficf {
                let n = fic_len(mid);
                fic = v.get(pos..pos + n)?.to_vec();
                pos += n;
            }
            let _ = pos;
        } else if &name[..3] == b"est" {
            if v.len() < 3 {
                continue;
            }
            let scid = v[0] >> 2;
            streams.push((scid, v[3..].to_vec()));
        }
    }
    if !deti {
        return None;
    }
    Some(EtiFrame { fic, streams })
}

fn tag(out: &mut Vec<u8>, name: &[u8; 4], value: &[u8]) {
    out.extend_from_slice(name);
    out.extend_from_slice(&((value.len() * 8) as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// Build an EDI AF packet with *ptr, deti and est tags.
pub fn build_af(seq: u16, fct: u8, fic: &[u8], streams: &[Stream]) -> Vec<u8> {
    let mut payload = Vec::new();
    tag(&mut payload, b"*ptr", &[b'D', b'E', b'T', b'I', 0, 0, 0, 0]);
    let mut deti = vec![0x40, fct, 0xFF, (1 << 6) | ((fct % 8) << 3), 0xFF, 0xFF];
    deti.extend_from_slice(fic);
    tag(&mut payload, b"deti", &deti);
    for (n, s) in streams.iter().enumerate() {
        let mut v = vec![(s.scid << 2) | ((s.sad >> 8) as u8 & 0x03), s.sad as u8, s.tpl << 2];
        v.extend_from_slice(s.data);
        tag(&mut payload, &[b'e', b's', b't', n as u8 + 1], &v);
    }
    let mut p = Vec::with_capacity(payload.len() + 12);
    p.extend_from_slice(b"AF");
    p.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.push(0x90); // CRC present, revision 1.0
    p.push(b'T');
    p.extend_from_slice(&payload);
    let crc = crc16_ccitt(&p);
    p.extend_from_slice(&crc.to_be_bytes());
    p
}

/// Splits a byte stream (TCP) into ETI(NI) frames or EDI AF packets.
pub struct StreamParser {
    edi: bool,
    buf: Vec<u8>,
    /// Bytes skipped to find the frame start (resynchronisation).
    pub skipped: u64,
}

impl StreamParser {
    pub fn new(edi: bool) -> Self {
        StreamParser { edi, buf: Vec::with_capacity(4 * ETI_FRAME), skipped: 0 }
    }

    pub fn push(&mut self, data: &[u8], mut on_frame: impl FnMut(EtiFrame)) {
        self.buf.extend_from_slice(data);
        let mut pos = 0;
        loop {
            let rest = &self.buf[pos..];
            if self.edi {
                let Some(i) = rest.windows(2).position(|w| w == b"AF") else {
                    let keep = rest.len().min(1);
                    self.skipped += (rest.len() - keep) as u64;
                    pos = self.buf.len() - keep;
                    break;
                };
                let r = &rest[i..];
                if r.len() < 10 {
                    self.skipped += i as u64;
                    pos += i;
                    break;
                }
                let len = u32::from_be_bytes([r[2], r[3], r[4], r[5]]) as usize;
                let total = 10 + len + if r[8] & 0x80 != 0 { 2 } else { 0 };
                if len > 1 << 20 || r[9] != b'T' {
                    self.skipped += i as u64 + 1;
                    pos += i + 1;
                    continue;
                }
                if r.len() < total {
                    self.skipped += i as u64;
                    pos += i;
                    break;
                }
                match parse_af(&r[..total]) {
                    Some(f) => {
                        on_frame(f);
                        self.skipped += i as u64;
                        pos += i + total;
                    }
                    None => {
                        self.skipped += i as u64 + 1;
                        pos += i + 1;
                    }
                }
            } else {
                let Some(i) = rest.windows(3).position(|w| w == FSYNC_EVEN || w == FSYNC_ODD) else {
                    let keep = rest.len().min(3);
                    self.skipped += (rest.len() - keep) as u64;
                    pos = self.buf.len() - keep;
                    break;
                };
                // the frame starts with the ERR byte before FSYNC
                let start = i.saturating_sub(1);
                let r = &rest[start..];
                if r.len() < 8 {
                    pos += start;
                    break;
                }
                let fl = ((((r[6] & 0x07) as usize) << 8) | r[7] as usize) * 4;
                let need = 8 + fl;
                if r.len() < need {
                    pos += start;
                    break;
                }
                match parse_eti(&r[..need]) {
                    Some(f) => {
                        on_frame(f);
                        self.skipped += start as u64;
                        // padded frames are 6144 bytes, streamed ones end here
                        pos += start + need;
                    }
                    None => {
                        self.skipped += start as u64 + 1;
                        pos += start + 1;
                    }
                }
            }
        }
        self.buf.drain(..pos.min(self.buf.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_streams_than_the_frame_holds() {
        // 127 streams announced, a frame length of 4 bytes
        let mut f = vec![0u8; 16];
        f[0] = 0xFF;
        f[1..4].copy_from_slice(&FSYNC_EVEN);
        f[5] = 0x7F;
        f[7] = 1;
        assert!(parse_eti(&f).is_none());
    }

    fn sample() -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut fic = vec![0u8; 96];
        for (i, fib) in fic.chunks_exact_mut(32).enumerate() {
            fib[0] = 0x05 + i as u8;
            let crc = crc16_ccitt(&fib[..30]);
            fib[30..].copy_from_slice(&crc.to_be_bytes());
        }
        let streams = vec![(0..288).map(|x| x as u8).collect(), (0..96).map(|x| (x * 3) as u8).collect()];
        (fic, streams)
    }

    #[test]
    fn protection_in_the_stc() {
        // UEP table index 7: 48 kbps at protection level 3; EEP 3-A, EEP 2-B
        assert_eq!(uep_info(7).map(|i| i.2), Some(3));
        assert_eq!(tpl(Protection::Uep { index: 7 }), 0x12);
        assert_eq!(tpl(Protection::Eep { option: 0, level: 2 }), 0x22);
        assert_eq!(tpl(Protection::Eep { option: 1, level: 1 }), 0x25);
    }

    #[test]
    fn eti_roundtrip_and_stream() {
        let (fic, data) = sample();
        let streams: Vec<Stream> =
            data.iter().enumerate().map(|(i, d)| Stream { scid: i as u8 + 1, sad: i as u16 * 96, tpl: 0x22, data: d }).collect();
        let f = build_eti(3, &fic, &streams);
        assert_eq!(f.len(), ETI_FRAME);
        let p = parse_eti(&f).unwrap();
        assert_eq!(p.fic, fic);
        assert_eq!(p.streams[1], (2, data[1].clone()));
        let mut parser = StreamParser::new(false);
        let mut n = 0;
        let mut bytes = vec![1, 2, 3];
        bytes.extend_from_slice(&build_eti(4, &fic, &streams));
        bytes.extend_from_slice(&f);
        for chunk in bytes.chunks(1000) {
            parser.push(chunk, |fr| {
                assert_eq!(fr.streams[0].1, data[0]);
                n += 1;
            });
        }
        assert_eq!(n, 2);
    }

    #[test]
    fn edi_roundtrip_and_stream() {
        let (fic, data) = sample();
        let streams: Vec<Stream> = data.iter().enumerate().map(|(i, d)| Stream { scid: i as u8 + 5, sad: 0, tpl: 0x22, data: d }).collect();
        let p = build_af(7, 9, &fic, &streams);
        let f = parse_af(&p).unwrap();
        assert_eq!(f.fic, fic);
        assert_eq!(f.streams[0], (5, data[0].clone()));
        let mut parser = StreamParser::new(true);
        let mut n = 0;
        let mut bytes = b"xxAFjunk".to_vec();
        for k in 0..3 {
            bytes.extend_from_slice(&build_af(k, k as u8, &fic, &streams));
        }
        for chunk in bytes.chunks(333) {
            parser.push(chunk, |fr| {
                assert_eq!(fr.streams[1].1, data[1]);
                n += 1;
            });
        }
        assert_eq!(n, 3);
    }
}
