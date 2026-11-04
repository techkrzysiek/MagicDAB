//! Recording the whole multiplex as ETI(NI) frames (eti.rs), one per CIF:
//! the FIC and every sub-channel decoded. The receiver decodes the stations
//! listened to, the texts of the other stations and the data services
//! anyway; the sub-channels left are decoded here. An ETI file is a
//! fortieth of the IQ samples (about 15 MB a minute), plays back in this
//! program and in DABlin or the ODR tools.

use crate::consts::CIFS_PER_FRAME;
use crate::ensemble::Ensemble;
use crate::eti::{build_eti, tpl, Stream, ETI_FRAME};
use crate::msc::SubchannelDecoder;
use std::collections::VecDeque;

/// CIFs between the FIC of a logical frame and its sub-channel data at the
/// output of the time deinterleaver (msc.rs keeps 16 CIFs).
pub const DELAY: usize = 16;
/// FIC bytes of one CIF: 3 FIBs of 32 bytes (mode I).
pub const FIC_PER_CIF: usize = 96;
/// Bytes of a frame besides the STC and the MST: ERR and FSYNC, FC, EOH,
/// EOF, TIST.
const FRAME_OVERHEAD: usize = 20;

pub struct EtiRecorder {
    /// The FIC of the last CIFs, oldest first: a frame is written once the
    /// sub-channel data of its FIC has come out of the deinterleaver.
    fics: VecDeque<Vec<u8>>,
    /// Decoded sub-channels (id, data) of the CIFs of the frame in hand.
    cifs: Vec<Vec<(u8, Vec<u8>)>>,
    /// Decoders of the sub-channels nothing else in the receiver decodes.
    extra: Vec<(u8, SubchannelDecoder)>,
    /// Frame count of the frames written (FCT, modulo 250).
    fct: u8,
    /// Frames written.
    pub frames: u64,
}

impl Default for EtiRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl EtiRecorder {
    pub fn new() -> Self {
        EtiRecorder {
            fics: VecDeque::with_capacity(DELAY + CIFS_PER_FRAME),
            cifs: vec![Vec::new(); CIFS_PER_FRAME],
            extra: Vec::new(),
            fct: 0,
            frames: 0,
        }
    }

    /// Sub-channels decoded here: every one of the ensemble that none of
    /// `covered` (the sub-channels of the other decoders) is.
    pub fn update(&mut self, ens: &Ensemble, covered: &[u8]) {
        self.extra.retain(|(id, d)| !covered.contains(id) && ens.subchannels.get(id).is_some_and(|sc| *sc == d.subch));
        for sc in ens.subchannels.values() {
            if covered.contains(&sc.id) || self.extra.iter().any(|(id, _)| *id == sc.id) {
                continue;
            }
            if let Some(d) = SubchannelDecoder::new(sc) {
                self.extra.push((sc.id, d));
            }
        }
    }

    /// Sub-channels decoded here.
    pub fn extra_ids(&self) -> Vec<u8> {
        let mut ids: Vec<u8> = self.extra.iter().map(|(id, _)| *id).collect();
        ids.sort();
        ids
    }

    /// The FIBs of a frame as decoded (fic.rs `raw`: CIFS_PER_FRAME times
    /// FIC_PER_CIF bytes), before its CIFs are added.
    pub fn fic(&mut self, raw: &[u8]) {
        for c in 0..CIFS_PER_FRAME {
            let mut f = raw.get(c * FIC_PER_CIF..(c + 1) * FIC_PER_CIF).map(|f| f.to_vec()).unwrap_or_default();
            // (missing: FIBs of zeros, whose CRC no reader accepts)
            f.resize(FIC_PER_CIF, 0);
            self.fics.push_back(f);
        }
    }

    /// The decoded data of a sub-channel in CIF `cif` of the frame (from any
    /// decoder of the receiver; the same sub-channel twice counts once).
    pub fn add(&mut self, cif: usize, subch: u8, data: &[u8]) {
        let Some(c) = self.cifs.get_mut(cif) else { return };
        if !c.iter().any(|(id, _)| *id == subch) {
            c.push((subch, data.to_vec()));
        }
    }

    /// Decodes the sub-channels of this recorder from the soft bits of one CIF.
    pub fn decode(&mut self, cif: usize, soft: &[i8]) {
        for (id, dec) in self.extra.iter_mut() {
            if let Some(data) = dec.push_cif(soft) {
                if let Some(c) = self.cifs.get_mut(cif) {
                    if !c.iter().any(|(x, _)| x == id) {
                        c.push((*id, data.to_vec()));
                    }
                }
            }
        }
    }

    /// The ETI frames of the frame in hand (none for the first DELAY CIFs of
    /// a recording), with the sub-channels at the start addresses and
    /// protection the ensemble gives them now.
    pub fn frames(&mut self, ens: &Ensemble, out: &mut Vec<Vec<u8>>) {
        for c in 0..CIFS_PER_FRAME {
            let mut data = std::mem::take(&mut self.cifs[c]);
            if self.fics.len() <= DELAY {
                continue;
            }
            let fic = self.fics.pop_front().unwrap_or_default();
            data.sort_by_key(|(id, _)| *id);
            let mut streams: Vec<Stream> = data
                .iter()
                .filter_map(|(id, d)| {
                    ens.subchannels.get(id).map(|sc| Stream { scid: *id, sad: sc.start, tpl: tpl(sc.protection), data: d })
                })
                .collect();
            // (a corrupted configuration may claim more than a frame holds:
            // the biggest sub-channels are left out rather than a broken frame)
            while !streams.is_empty() && frame_len(fic.len(), &streams) > ETI_FRAME {
                let biggest = streams.iter().enumerate().max_by_key(|(_, s)| s.data.len()).map(|(i, _)| i).unwrap();
                streams.remove(biggest);
            }
            out.push(build_eti(self.fct, &fic, &streams));
            self.fct = (self.fct + 1) % 250;
            self.frames += 1;
        }
    }
}

impl EtiRecorder {
    /// The frame of an ETI or EDI stream as it came: its FIC and sub-channels
    /// belong together already (no deinterleaver to wait for).
    pub fn stream(&mut self, ens: &Ensemble, fic: &[u8], data: &[(u8, Vec<u8>)], out: &mut Vec<Vec<u8>>) {
        let mut streams: Vec<Stream> = data
            .iter()
            .filter_map(|(id, d)| ens.subchannels.get(id).map(|sc| Stream { scid: *id, sad: sc.start, tpl: tpl(sc.protection), data: d }))
            .collect();
        streams.sort_by_key(|s| s.scid);
        while !streams.is_empty() && frame_len(fic.len(), &streams) > ETI_FRAME {
            let biggest = streams.iter().enumerate().max_by_key(|(_, s)| s.data.len()).map(|(i, _)| i).unwrap();
            streams.remove(biggest);
        }
        out.push(build_eti(self.fct, fic, &streams));
        self.fct = (self.fct + 1) % 250;
        self.frames += 1;
    }
}

/// Bytes of a frame before padding: header, STC, EOH, MST, EOF, TIST.
fn frame_len(fic: usize, streams: &[Stream]) -> usize {
    FRAME_OVERHEAD + 4 * streams.len() + fic + streams.iter().map(|s| s.data.len()).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{CIF_BITS, CU_BITS};
    use crate::crc::{crc16_ccitt, prbs};
    use crate::ensemble::Subchannel;
    use crate::eti::{parse_eti, StreamParser};
    use crate::puncture::{subchannel_pattern, Protection};
    use crate::viterbi::encode;

    fn subchannel(id: u8, start: u16, size: u16, bitrate: u16, level: u8) -> Subchannel {
        Subchannel {
            id,
            start,
            size,
            protection: Protection::Eep { option: 0, level },
            bitrate,
            protection_label: String::new(),
            code_rate: String::new(),
            fec_scheme: 0,
        }
    }

    fn ensemble(subchannels: &[Subchannel]) -> Ensemble {
        let mut e = Ensemble { eid: Some(0x1234), ..Default::default() };
        for sc in subchannels {
            e.subchannels.insert(sc.id, sc.clone());
        }
        e
    }

    /// The FIBs of frame `f`: valid CRCs, the CIF number in the first byte.
    fn fibs(f: usize) -> Vec<u8> {
        let mut raw = vec![0u8; CIFS_PER_FRAME * FIC_PER_CIF];
        for c in 0..CIFS_PER_FRAME {
            for fib in raw[c * FIC_PER_CIF..(c + 1) * FIC_PER_CIF].chunks_exact_mut(32) {
                fib[0] = (f * CIFS_PER_FRAME + c) as u8;
                fib[1] = 0xFF;
                let crc = crc16_ccitt(&fib[..30]);
                fib[30..].copy_from_slice(&crc.to_be_bytes());
            }
        }
        raw
    }

    /// Data of logical frame `k` of a sub-channel as a decoder gives it.
    fn logical(k: usize, bitrate: u16) -> Vec<u8> {
        (0..bitrate as usize * 3).map(|i| (k + i) as u8).collect()
    }

    #[test]
    fn frames_carry_the_fic_of_their_logical_frame() {
        // two sub-channels decoded elsewhere in the receiver, their data
        // arrives DELAY CIFs after the FIC of the same logical frame
        let ens = ensemble(&[subchannel(1, 0, 48, 64, 2), subchannel(3, 48, 36, 48, 2)]);
        let mut rec = EtiRecorder::new();
        rec.update(&ens, &[1, 3]);
        assert!(rec.extra_ids().is_empty());
        let mut out = Vec::new();
        for f in 0..8 {
            rec.fic(&fibs(f));
            for c in 0..CIFS_PER_FRAME {
                let k = f * CIFS_PER_FRAME + c;
                if k >= DELAY {
                    rec.add(c, 3, &logical(k - DELAY, 48));
                    rec.add(c, 1, &logical(k - DELAY, 64));
                    // (the same sub-channel from a second decoder: once)
                    rec.add(c, 1, &logical(99, 64));
                }
            }
            rec.frames(&ens, &mut out);
        }
        // CIFs 16..31: a frame each, logical frames 0..15
        assert_eq!(out.len(), 16);
        assert_eq!(rec.frames, 16);
        for (n, frame) in out.iter().enumerate() {
            assert_eq!(frame.len(), ETI_FRAME);
            assert_eq!(frame[4] as usize, n, "FCT counts the frames");
            let p = parse_eti(frame).unwrap_or_else(|| panic!("frame {n} parses"));
            assert_eq!(p.fic.len(), FIC_PER_CIF);
            assert_eq!(p.fic[0] as usize, n, "the FIC of logical frame {n}");
            assert_eq!(p.streams, vec![(1, logical(n, 64)), (3, logical(n, 48))]);
        }
        // the STC says where each sub-channel is and how it is protected
        assert_eq!(&out[0][8..12], &[1 << 2, 0, 0x22 << 2, 24], "SCID 1 at CU 0, EEP 3-A, 24 words");
        assert_eq!(&out[0][12..16], &[3 << 2, 48, 0x22 << 2, 18]);
        // a stream of the frames parses as the ETI input does
        let mut parser = StreamParser::new(false);
        let mut n = 0;
        let bytes: Vec<u8> = out.iter().flatten().copied().collect();
        for chunk in bytes.chunks(5000) {
            parser.push(chunk, |_| n += 1);
        }
        assert_eq!(n, 16);
    }

    #[test]
    fn decoders_for_the_sub_channels_nobody_decodes() {
        let ens = ensemble(&[subchannel(1, 0, 48, 64, 2), subchannel(2, 48, 48, 64, 2), subchannel(3, 96, 36, 48, 2)]);
        let mut rec = EtiRecorder::new();
        rec.update(&ens, &[1]);
        assert_eq!(rec.extra_ids(), vec![2, 3]);
        // a listener chose the station of sub-channel 2: its decoder covers it now
        rec.update(&ens, &[1, 2]);
        assert_eq!(rec.extra_ids(), vec![3]);
        // and left again
        rec.update(&ens, &[1]);
        assert_eq!(rec.extra_ids(), vec![2, 3]);
        // a reconfiguration: sub-channel 3 gone, 2 changed (a new decoder)
        let mut ens2 = ensemble(&[subchannel(1, 0, 48, 64, 2), subchannel(2, 48, 72, 96, 2)]);
        ens2.subchannels.remove(&3);
        rec.update(&ens2, &[1]);
        assert_eq!(rec.extra_ids(), vec![2]);
        assert_eq!(rec.extra[0].1.subch.bitrate, 96);
    }

    /// Time interleaving delays of EN 300 401 (CIFs, by bit index modulo 16).
    const DELAYS: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

    #[test]
    fn a_coded_sub_channel_comes_out_aligned_with_its_fic() {
        // EEP 3-A at 8 kbps: 6 CUs, 192 information bits a logical frame
        let sc = subchannel(5, 10, 6, 8, 2);
        let ens = ensemble(std::slice::from_ref(&sc));
        let pattern = subchannel_pattern(sc.protection, sc.size, sc.bitrate).unwrap();
        assert_eq!(pattern.info_bits(), 192);
        let disp = prbs(192);
        // the logical frames coded as on air
        let frames_on_air = 28;
        let coded: Vec<Vec<u8>> = (0..frames_on_air)
            .map(|r| {
                let bytes = logical(r, 8);
                let bits: Vec<u8> =
                    bytes.iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1)).zip(&disp).map(|(b, p)| b ^ p).collect();
                encode(&bits).iter().zip(&pattern.keep).filter(|(_, &k)| k).map(|(&c, _)| c).collect()
            })
            .collect();
        assert_eq!(coded[0].len(), sc.size as usize * CU_BITS);
        let mut rec = EtiRecorder::new();
        rec.update(&ens, &[]);
        assert_eq!(rec.extra_ids(), vec![5]);
        let mut out = Vec::new();
        let mut cif = vec![0i8; CIF_BITS];
        for f in 0..frames_on_air / CIFS_PER_FRAME {
            rec.fic(&fibs(f));
            for c in 0..CIFS_PER_FRAME {
                let k = f * CIFS_PER_FRAME + c;
                // CIF k carries bit i of logical frame k - delay(i)
                let start = sc.start as usize * CU_BITS;
                for i in 0..coded[0].len() {
                    let d = DELAYS[i & 15];
                    cif[start + i] = if k >= d {
                        if coded[k - d][i] == 1 {
                            -64
                        } else {
                            64
                        }
                    } else {
                        0
                    };
                }
                rec.decode(c, &cif);
            }
            rec.frames(&ens, &mut out);
        }
        assert_eq!(out.len(), frames_on_air - DELAY);
        for (n, frame) in out.iter().enumerate() {
            let p = parse_eti(frame).unwrap();
            assert_eq!(p.fic[0] as usize, n);
            assert_eq!(p.streams, vec![(5, logical(n, 8))], "logical frame {n} with its FIC");
        }
    }

    #[test]
    fn a_configuration_too_big_for_a_frame_loses_its_biggest_sub_channels() {
        // 12 sub-channels of 184 kbps claimed (6624 bytes a CIF): more than a
        // frame holds; ten of them fit
        let scs: Vec<Subchannel> = (0..12u8).map(|i| subchannel(i, i as u16 * 72, 72, 184, 3)).collect();
        let ens = ensemble(&scs);
        let mut rec = EtiRecorder::new();
        for f in 0..5 {
            rec.fic(&fibs(f));
            for c in 0..CIFS_PER_FRAME {
                for sc in &scs {
                    rec.add(c, sc.id, &logical(f, 184));
                }
            }
        }
        let mut out = Vec::new();
        rec.frames(&ens, &mut out);
        assert_eq!(out.len(), CIFS_PER_FRAME);
        let p = parse_eti(&out[0]).unwrap();
        assert_eq!(p.streams.len(), 10);
        assert_eq!(out[0].len(), ETI_FRAME);
    }
}
