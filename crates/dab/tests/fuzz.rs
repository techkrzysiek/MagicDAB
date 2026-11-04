//! Random and corrupted input must never make a decoder panic: the data
//! comes from the air (bit errors pass CRCs now and then) or from the network.

use magicdab_dab::{dls, eti, fig, mot, packet, pad, spi};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Small deterministic generator (no dependencies).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() % (max as u64 + 1)) as usize;
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Run `f` on many random inputs; on a panic, report the input.
fn fuzz(name: &str, rounds: usize, max: usize, mut f: impl FnMut(&[u8], &mut Rng)) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ name.len() as u64);
    for i in 0..rounds {
        let data = rng.bytes(max);
        let mut r2 = Rng(rng.next() | 1);
        if catch_unwind(AssertUnwindSafe(|| f(&data, &mut r2))).is_err() {
            panic!("{name}: panic in round {i} on {} bytes: {:02X?}", data.len(), &data[..data.len().min(64)]);
        }
    }
}

#[test]
fn figs() {
    let mut p = fig::FigParser::new();
    fuzz("fib", 200_000, 30, |d, _| {
        let mut fib = d.to_vec();
        fib.resize(30, 0);
        p.parse_fib(&fib)
    });
}

#[test]
fn mot_objects() {
    fuzz("data group", 100_000, 300, |d, _| {
        let _ = mot::parse_data_group(d);
    });
    fuzz("mot header", 100_000, 200, |d, _| {
        let _ = mot::parse_header(d);
    });
    let mut dec = mot::MotDecoder::new();
    let mut out = Vec::new();
    fuzz("mot decoder", 100_000, 400, |d, _| {
        dec.process(d, &mut out);
        out.clear();
    });
}

#[test]
fn pad_and_labels() {
    let mut p = pad::PadDecoder::new();
    p.register_app(12);
    p.register_app(13);
    let mut out = Vec::new();
    fuzz("x-pad", 100_000, 200, |d, r| {
        let fpad = [r.next() as u8, r.next() as u8];
        p.process(d, r.next() % 2 == 0, fpad, &mut out);
        out.clear();
    });
    fuzz("dab+ pad", 100_000, 300, |d, _| {
        let _ = pad::dabplus_pad(d);
    });
    fuzz("mp2 pad", 100_000, 300, |d, r| {
        let _ = pad::mp2_pad(d, (r.next() % 8) as usize);
    });
    let mut dls = dls::DlsDecoder::new();
    fuzz("dls", 100_000, 200, |d, _| {
        let _ = dls.process(d);
    });
}

#[test]
fn spi_documents() {
    fuzz("spi binary", 100_000, 600, |d, r| {
        let _ = spi::decode(d, r.next() as u32);
    });
    fuzz("spi time", 50_000, 12, |d, _| {
        let _ = spi::time_point(d);
    });
    fuzz("bearer", 50_000, 40, |d, _| {
        let _ = spi::parse_bearer(&String::from_utf8_lossy(d));
    });
}

#[test]
fn streams() {
    fuzz("eti frame", 50_000, 6144, |d, _| {
        let _ = eti::parse_eti(d);
    });
    for edi in [false, true] {
        let mut sp = eti::StreamParser::new(edi);
        // pieces of a stream: tags and frames cut anywhere
        fuzz("stream", 50_000, 3000, |d, r| {
            let mut d = d.to_vec();
            if edi && r.next() % 3 == 0 {
                // an AF packet header now and then
                let h = [b'A', b'F', 0, 0, 0, 40, 0, 0, 0x90, b'T'];
                d.splice(0..0, h);
            }
            sp.push(&d, |_| {})
        });
    }
    let mut pk = packet::PacketDecoder::new();
    fuzz("packets", 100_000, 400, |d, _| pk.push(d, |_, _| {}));
}
