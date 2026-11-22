//! Decode TPEG from a recording of a packet mode sub-channel and print the
//! services, statistics and traffic messages.
//!
//! The recording is a sequence of logical frames of the sub-channel, each
//! one preceded by its length as a 16 bit big endian number.
//!
//!   cargo run -p magicdab-dab --example tpeg_dump -- <file> [--address N] [--raw] [--json]
//!
//! `--address`: packet address of the TPEG component (default 2);
//! `--raw`: the packets carry the stream without MSC data groups;
//! `--json`: print the messages as JSON, one per line;
//! `--start T`: the recording began at T (seconds since 1970 UTC): messages
//! expire as they would have live, 24 ms per logical frame.

use magicdab_dab::packet::PacketDecoder;
use magicdab_dab::tpeg::{TpegDecoder, TrafficEvent};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: tpeg_dump <file> [--address N] [--raw] [--json]");
        std::process::exit(2);
    };
    let address: u16 = args.iter().position(|a| a == "--address").and_then(|i| args.get(i + 1)).and_then(|a| a.parse().ok()).unwrap_or(2);
    let raw = args.iter().any(|a| a == "--raw");
    let json = args.iter().any(|a| a == "--json");
    let start: Option<f64> = args.iter().position(|a| a == "--start").and_then(|i| args.get(i + 1)).and_then(|a| a.parse().ok());
    let data = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });

    let mut packets = PacketDecoder::new();
    let mut tpeg = TpegDecoder::new();
    let mut frames = 0;
    let (mut changes, mut removals) = (0, 0);
    let mut i = 0;
    while i + 2 <= data.len() {
        let n = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        let Some(frame) = data.get(i + 2..i + 2 + n) else { break };
        i += 2 + n;
        frames += 1;
        packets.push(frame, |addr, group| {
            if addr != address {
                return;
            }
            if raw {
                tpeg.push(&group);
            } else {
                tpeg.push_data_group(&group);
            }
        });
        if frames % 42 == 0 {
            if let Some(t) = start {
                tpeg.expire((t + frames as f64 * 0.024) as u64);
            }
            for k in tpeg.take_changed() {
                changes += 1;
                removals += tpeg.get(&k).is_none() as u32;
            }
        }
    }

    println!(
        "{frames} logical frames ({:.0} s), {} packets, {} packet CRC errors",
        frames as f64 * 0.024,
        packets.packets,
        packets.crc_errors
    );
    for s in tpeg.services() {
        println!("service {} {:?} {:?}", s.sid, s.name.unwrap_or_default(), s.description.unwrap_or_default());
        for c in s.components {
            println!(
                "  SCID {:3} AID {:3} {:4} version {:5} content {:?}{}",
                c.scid,
                c.aid,
                c.application,
                c.version.unwrap_or_default(),
                c.content.unwrap_or_default(),
                if c.encrypted { " encrypted" } else { "" }
            );
        }
    }
    println!("{}", serde_json::to_string_pretty(tpeg.stats()).unwrap());

    let events: Vec<&TrafficEvent> = tpeg.messages().collect();
    println!("{} messages; {changes} changes reported, {removals} of them removals", events.len());
    for ev in events {
        if json {
            println!("{}", serde_json::to_string(ev).unwrap());
        } else {
            print_event(ev);
        }
    }
}

fn print_event(ev: &TrafficEvent) {
    let effect = ev.effect.map(|e| format!("{} ({})", e.name, e.code)).unwrap_or_default();
    let causes: Vec<String> = ev
        .causes
        .iter()
        .map(|c| match c.sub_name {
            Some(s) => format!("{} / {} ({}/{})", c.name, s, c.code, c.sub_code.unwrap_or(0)),
            None => format!("{} ({})", c.name, c.code),
        })
        .collect();
    let warning = ev.warning.map(|w| w.name).unwrap_or("-");
    println!("{} v{} {effect}; {}; {warning}", ev.key, ev.version, causes.join(", "));
    if let Some(l) = &ev.location {
        let pts: Vec<String> = l.points.iter().map(|p| format!("{:.5},{:.5}", p[0], p[1])).collect();
        let tmc = l
            .tmc
            .as_ref()
            .map(|t| format!(" TMC {:X}:{}:{}{}", t.country, t.table, t.location, if t.positive { "+" } else { "-" }))
            .unwrap_or_default();
        println!(
            "    {} {} {} [{}]{tmc}",
            l.methods.join("+"),
            l.kind.unwrap_or("-"),
            l.point.map(|p| format!("at {:.5},{:.5}", p[0], p[1])).unwrap_or_default(),
            pts.join(" ")
        );
    }
    for a in &ev.advice {
        println!("    advice: {} {}", a.name.unwrap_or("-"), a.sub_name.unwrap_or(""));
    }
}
