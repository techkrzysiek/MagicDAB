//! Serve a raw IQ recording as an EDI or ETI stream over TCP (like the TCP
//! outputs of ODR-DabMux), for testing the ETI / EDI input.
//!
//! usage: etiserve <file.uff> <port> [--eti]

use magicdab_dab::consts::{CIFS_PER_FRAME, CIF_BITS};
use magicdab_dab::crc::crc16_ccitt;
use magicdab_dab::eti::{build_af, build_eti, Stream};
use magicdab_dab::fic::FicDecoder;
use magicdab_dab::fig::FigParser;
use magicdab_dab::msc::SubchannelDecoder;
use magicdab_dab::ofdm::{Frame, FrameSink, OfdmDemodulator, SampleSource};
use num_complex::Complex32;
use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

struct FileSource {
    f: std::fs::File,
    offset: u64,
    buf: Vec<u8>,
}

impl SampleSource for FileSource {
    fn read(&mut self, out: &mut [Complex32]) -> bool {
        self.buf.resize(out.len() * 2, 0);
        let mut got = 0;
        while got < self.buf.len() {
            match self.f.read(&mut self.buf[got..]) {
                Ok(0) => {
                    // loop the recording
                    if self.f.seek(std::io::SeekFrom::Start(self.offset)).is_err() {
                        return false;
                    }
                }
                Ok(n) => got += n,
                Err(_) => return false,
            }
        }
        for (o, c) in out.iter_mut().zip(self.buf.chunks_exact(2)) {
            *o = Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5);
        }
        true
    }
}

struct Server {
    eti: bool,
    listener: TcpListener,
    client: Option<TcpStream>,
    fic: FicDecoder,
    figs: FigParser,
    subch: BTreeMap<u8, (u16, SubchannelDecoder)>,
    fct: u8,
    seq: u16,
    next: Instant,
}

impl FrameSink for Server {
    fn frame(&mut self, frame: &Frame) {
        // FIBs of the 4 CIFs (failed ones replaced by empty FIBs)
        let mut fibs: Vec<Vec<u8>> = Vec::new();
        let figs = &mut self.figs;
        self.fic.decode(&frame.fic, |fib| {
            figs.parse_fib(fib);
            let mut f = fib.to_vec();
            f.extend_from_slice(&crc16_ccitt(fib).to_be_bytes());
            fibs.push(f);
        });
        while fibs.len() < 12 {
            let mut f = vec![0xFF; 30];
            f.extend_from_slice(&crc16_ccitt(&f).to_be_bytes());
            fibs.push(f);
        }
        // decoders for all sub-channels of the ensemble
        for sc in self.figs.ensemble.subchannels.values() {
            if let std::collections::btree_map::Entry::Vacant(e) = self.subch.entry(sc.id) {
                if let Some(d) = SubchannelDecoder::new(sc) {
                    e.insert((sc.start, d));
                }
            }
        }
        if self.client.is_none() {
            self.listener.set_nonblocking(true).ok();
            if let Ok((c, a)) = self.listener.accept() {
                eprintln!("client {a}");
                c.set_nodelay(true).ok();
                self.client = Some(c);
            }
        }
        for c in 0..CIFS_PER_FRAME {
            let cif = &frame.msc[c * CIF_BITS..(c + 1) * CIF_BITS];
            let mut data: Vec<(u8, u16, Vec<u8>)> = Vec::new();
            for (&id, (start, d)) in self.subch.iter_mut() {
                if let Some(out) = d.push_cif(cif) {
                    data.push((id, *start, out.to_vec()));
                }
            }
            let streams: Vec<Stream> = data.iter().map(|(id, sad, d)| Stream { scid: *id, sad: *sad, tpl: 0x22, data: d }).collect();
            let fic: Vec<u8> = fibs[c * 3..c * 3 + 3].concat();
            let bytes = if self.eti { build_eti(self.fct, &fic, &streams) } else { build_af(self.seq, self.fct, &fic, &streams) };
            self.fct = ((self.fct as u16 + 1) % 250) as u8;
            self.seq = self.seq.wrapping_add(1);
            // real time: one frame per 24 ms
            self.next += Duration::from_millis(24);
            let now = Instant::now();
            if self.next > now {
                std::thread::sleep(self.next - now);
            } else if now - self.next > Duration::from_secs(1) {
                self.next = now;
            }
            if let Some(cl) = self.client.as_mut() {
                if cl.write_all(&bytes).is_err() {
                    eprintln!("client gone");
                    self.client = None;
                }
            }
        }
    }

    fn sync_state(&mut self, synced: bool, _signal: bool) {
        eprintln!("sync {synced}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: etiserve <file.uff> <port> [--eti]");
        std::process::exit(1);
    }
    let mut f = std::fs::File::open(&args[1]).expect("open");
    let mut head = [0u8; 5];
    f.read_exact(&mut head).ok();
    let offset = if &head == b"<?xml" { 2048 } else { 0 };
    f.seek(std::io::SeekFrom::Start(offset)).unwrap();
    let listener = TcpListener::bind(("0.0.0.0", args[2].parse::<u16>().expect("port"))).expect("bind");
    let mut server = Server {
        eti: args.iter().any(|a| a == "--eti"),
        listener,
        client: None,
        fic: FicDecoder::new(),
        figs: FigParser::new(),
        subch: BTreeMap::new(),
        fct: 0,
        seq: 0,
        next: Instant::now(),
    };
    let mut src = FileSource { f, offset, buf: Vec::new() };
    let mut demod = OfdmDemodulator::new();
    let stop = AtomicBool::new(false);
    demod.run(&mut src, &mut server, &stop);
}
