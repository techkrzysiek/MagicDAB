//! MSC data groups and MOT object reassembly (EN 300 401 clause 5.3.3,
//! EN 301 234). Supports header mode (slideshow) and directory mode (SPI, CatSLS).

use crate::charset;
use crate::crc::check_crc16;
use std::collections::{HashMap, HashSet, VecDeque};

/// Parsed MSC data group.
#[derive(Debug)]
pub struct DataGroup<'a> {
    pub dg_type: u8,
    pub continuity: u8,
    pub repetition: u8,
    pub last: bool,
    pub segment: Option<u16>,
    pub transport_id: Option<u16>,
    pub data: &'a [u8],
}

pub fn parse_data_group(dg: &[u8]) -> Option<DataGroup<'_>> {
    if dg.len() < 2 {
        return None;
    }
    let ext = dg[0] & 0x80 != 0;
    let crc = dg[0] & 0x40 != 0;
    let seg_flag = dg[0] & 0x20 != 0;
    let ua_flag = dg[0] & 0x10 != 0;
    let dg_type = dg[0] & 0x0F;
    let continuity = dg[1] >> 4;
    let repetition = dg[1] & 0x0F;
    let end = if crc {
        if dg.len() < 4 || !check_crc16(dg) {
            return None;
        }
        dg.len() - 2
    } else {
        dg.len()
    };
    let mut i = 2;
    if ext {
        i += 2;
    }
    let mut last = false;
    let mut segment = None;
    if seg_flag {
        if i + 2 > end {
            return None;
        }
        last = dg[i] & 0x80 != 0;
        segment = Some((((dg[i] & 0x7F) as u16) << 8) | dg[i + 1] as u16);
        i += 2;
    }
    let mut transport_id = None;
    if ua_flag {
        if i + 1 > end {
            return None;
        }
        let tid_flag = dg[i] & 0x10 != 0;
        let len_ind = (dg[i] & 0x0F) as usize;
        i += 1;
        if tid_flag && len_ind >= 2 && i + 2 <= end {
            transport_id = Some(((dg[i] as u16) << 8) | dg[i + 1] as u16);
        }
        i += len_ind;
    }
    if i > end {
        return None;
    }
    Some(DataGroup { dg_type, continuity, repetition, last, segment, transport_id, data: &dg[i..end] })
}

#[derive(Clone, Debug, Default)]
pub struct MotObject {
    pub transport_id: u16,
    pub content_type: u8,
    pub content_subtype: u16,
    pub content_name: String,
    pub mime: Option<String>,
    /// All header extension parameters (id, data).
    pub params: Vec<(u8, Vec<u8>)>,
    pub body: Vec<u8>,
    /// Body size from the header core.
    pub body_size: usize,
}

impl MotObject {
    pub fn param(&self, id: u8) -> Option<&[u8]> {
        self.params.iter().find(|(p, _)| *p == id).map(|(_, d)| d.as_slice())
    }

    pub fn is_image(&self) -> bool {
        self.content_type == 2
    }

    pub fn mime_type(&self) -> String {
        if let Some(m) = &self.mime {
            return m.clone();
        }
        match (self.content_type, self.content_subtype) {
            (2, 0) => "image/gif",
            (2, 1) => "image/jpeg",
            (2, 2) => "image/bmp",
            (2, 3) => "image/png",
            (1, _) => "text/plain",
            (5, _) => "application/octet-stream",
            _ => "application/octet-stream",
        }
        .to_string()
    }
}

/// Bytes one object may take (slides and logos are far smaller, an SPI
/// programme guide a few hundred kilobytes), and all unfinished objects together.
const MAX_OBJECT: usize = 8 << 20;
const MAX_PENDING: usize = 32 << 20;

#[derive(Default)]
struct Segments {
    parts: HashMap<u16, Vec<u8>>,
    last: Option<u16>,
    size: usize,
}

impl Segments {
    fn add(&mut self, seg: u16, last: bool, data: &[u8]) {
        if !self.parts.contains_key(&seg) && self.size + data.len() <= MAX_OBJECT {
            self.size += data.len();
            self.parts.insert(seg, data.to_vec());
        }
        if last {
            self.last = Some(seg);
        }
    }

    fn complete(&self) -> Option<Vec<u8>> {
        let last = self.last?;
        if self.parts.len() <= last as usize || (0..=last).any(|s| !self.parts.contains_key(&s)) {
            return None;
        }
        let mut v = Vec::with_capacity(self.size);
        for s in 0..=last {
            v.extend_from_slice(&self.parts[&s]);
        }
        Some(v)
    }

    fn progress(&self) -> (usize, usize) {
        (self.parts.len(), self.last.map(|l| l as usize + 1).unwrap_or(0))
    }
}

#[derive(Default)]
struct Pending {
    header: Segments,
    body: Segments,
    parsed_header: Option<MotObject>,
}

/// Reception of one MOT object: body bytes received of `size`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectProgress {
    pub transport_id: u16,
    pub bytes: usize,
    pub size: usize,
    pub content_name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarouselProgress {
    /// Objects listed in the MOT directory (0: no directory, header mode).
    pub objects: usize,
    pub objects_done: usize,
    pub bytes: usize,
    pub bytes_done: usize,
    /// Segments of the objects being received now.
    pub segments: usize,
    pub segments_total: usize,
}

#[derive(Clone, Debug, Default)]
pub struct DirectoryEntry {
    pub transport_id: u16,
    pub header: MotObject,
}

/// Reassembles MOT objects from data groups of one MOT channel.
#[derive(Default)]
pub struct MotDecoder {
    pending: HashMap<u16, Pending>,
    /// Object of the latest header or body segment.
    last_tid: Option<u16>,
    /// Recently completed objects (transport id, body size, name): their
    /// repetitions do not count as being received.
    recent: VecDeque<(u16, usize, String)>,
    directory_segments: HashMap<u16, Segments>,
    pub directory: Option<(u16, Vec<DirectoryEntry>)>,
    /// Completed objects of the current directory (transport ids).
    pub carousel: HashSet<u16>,
    pub objects_done: u64,
    /// Bytes of all completed objects.
    pub bytes_done: usize,
}

/// Parse a MOT header (core + extension) into an object description.
pub fn parse_header(h: &[u8]) -> Option<(usize, MotObject)> {
    if h.len() < 7 {
        return None;
    }
    let body_size = ((h[0] as usize) << 20) | ((h[1] as usize) << 12) | ((h[2] as usize) << 4) | (h[3] as usize >> 4);
    let header_size = (((h[3] & 0x0F) as usize) << 9) | ((h[4] as usize) << 1) | (h[5] as usize >> 7);
    let content_type = (h[5] >> 1) & 0x3F;
    let content_subtype = (((h[5] & 0x01) as u16) << 8) | h[6] as u16;
    let mut obj = MotObject { content_type, content_subtype, body_size, ..Default::default() };
    let end = header_size.min(h.len());
    let mut i = 7;
    while i < end {
        let pli = h[i] >> 6;
        let id = h[i] & 0x3F;
        i += 1;
        let len = match pli {
            0 => 0,
            1 => 1,
            2 => 4,
            _ => {
                if i >= end {
                    break;
                }
                if h[i] & 0x80 != 0 {
                    if i + 1 >= end {
                        break;
                    }
                    let l = (((h[i] & 0x7F) as usize) << 8) | h[i + 1] as usize;
                    i += 2;
                    l
                } else {
                    let l = (h[i] & 0x7F) as usize;
                    i += 1;
                    l
                }
            }
        };
        if i + len > end {
            break;
        }
        let data = &h[i..i + len];
        i += len;
        match id {
            0x0C if !data.is_empty() => {
                obj.content_name = charset::decode(&data[1..], data[0] >> 4);
            }
            0x10 => {
                obj.mime = Some(String::from_utf8_lossy(data).to_string());
            }
            _ => {}
        }
        obj.params.push((id, data.to_vec()));
    }
    Some((body_size, obj))
}

impl MotDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Progress of a directory mode carousel (SPI, CatSLS): objects and bytes
    /// received of the current directory.
    pub fn carousel_progress(&self) -> Option<CarouselProgress> {
        let (segments, segments_total) = self.progress();
        let entries = match self.directory.as_ref() {
            Some((_, e)) if !e.is_empty() => e,
            _ => {
                // header mode (or an empty directory): only what arrived so far
                if self.objects_done == 0 && segments_total == 0 {
                    return None;
                }
                return Some(CarouselProgress {
                    objects_done: self.objects_done as usize,
                    bytes_done: self.bytes_done,
                    segments,
                    segments_total,
                    ..Default::default()
                });
            }
        };
        let mut p = CarouselProgress { objects: entries.len(), segments, segments_total, ..Default::default() };
        for e in entries {
            p.bytes += e.header.body_size;
            if self.carousel.contains(&e.transport_id) {
                p.objects_done += 1;
                p.bytes_done += e.header.body_size;
            } else if let Some(pending) = self.pending.get(&e.transport_id) {
                p.bytes_done += pending.body.size.min(e.header.body_size);
            }
        }
        Some(p)
    }

    /// Object being received in header mode (e.g. a slide), once its MOT header
    /// arrived.
    pub fn receiving(&self) -> Option<ObjectProgress> {
        let tid = self.last_tid?;
        let p = self.pending.get(&tid)?;
        let h = p.parsed_header.as_ref()?;
        if self.recent.iter().any(|(t, s, n)| *t == tid && *s == h.body_size && *n == h.content_name) {
            return None;
        }
        Some(ObjectProgress {
            transport_id: tid,
            bytes: p.body.size.min(h.body_size),
            size: h.body_size,
            content_name: h.content_name.clone(),
        })
    }

    /// Progress of objects being received: (received segments, total segments).
    pub fn progress(&self) -> (usize, usize) {
        let mut have = 0;
        let mut total = 0;
        for p in self.pending.values() {
            let (h, t) = p.body.progress();
            have += h;
            total += t;
        }
        (have, total)
    }

    /// Process one MSC data group; returns completed objects.
    pub fn process(&mut self, dg_bytes: &[u8], out: &mut Vec<MotObject>) {
        let Some(dg) = parse_data_group(dg_bytes) else { return };
        let (Some(tid), Some(seg)) = (dg.transport_id, dg.segment) else { return };
        // segmentation header: repetition count (3) + segment size (13)
        if dg.data.len() < 2 {
            return;
        }
        let seg_size = ((((dg.data[0] & 0x1F) as usize) << 8) | dg.data[1] as usize).min(dg.data.len() - 2);
        let payload = &dg.data[2..2 + seg_size];
        match dg.dg_type {
            3 => {
                self.last_tid = Some(tid);
                let p = self.pending.entry(tid).or_default();
                p.header.add(seg, dg.last, payload);
                if p.parsed_header.is_none() {
                    if let Some(h) = p.header.complete() {
                        if let Some((_, mut obj)) = parse_header(&h) {
                            obj.transport_id = tid;
                            p.parsed_header = Some(obj);
                        }
                    }
                }
                self.try_complete(tid, out);
            }
            4 => {
                self.last_tid = Some(tid);
                let p = self.pending.entry(tid).or_default();
                p.body.add(seg, dg.last, payload);
                self.try_complete(tid, out);
            }
            6 => {
                let s = self.directory_segments.entry(tid).or_default();
                s.add(seg, dg.last, payload);
                if let Some(d) = s.complete() {
                    self.directory_segments.remove(&tid);
                    self.parse_directory(tid, &d);
                    // objects that were received before the directory
                    let tids: Vec<u16> = self.pending.keys().copied().collect();
                    for t in tids {
                        self.try_complete(t, out);
                    }
                }
            }
            _ => {}
        }
        // limit memory: drop the partial objects when there are too many of them
        if self.pending.len() > 256 || self.pending.values().map(|p| p.header.size + p.body.size).sum::<usize>() > MAX_PENDING {
            self.pending.clear();
        }
        if self.directory_segments.len() > 16 {
            self.directory_segments.clear();
        }
    }

    fn parse_directory(&mut self, tid: u16, d: &[u8]) {
        if d.len() < 13 {
            return;
        }
        if self.directory.as_ref().map(|(t, _)| *t == tid).unwrap_or(false) {
            return;
        }
        let num_objects = ((d[4] as usize) << 8) | d[5] as usize;
        let ext_len = ((d[11] as usize) << 8) | d[12] as usize;
        let mut i = 13 + ext_len;
        let mut entries = Vec::with_capacity(num_objects.min(1024));
        for _ in 0..num_objects {
            if i + 2 + 7 > d.len() {
                break;
            }
            let otid = ((d[i] as u16) << 8) | d[i + 1] as u16;
            i += 2;
            let h = &d[i..];
            let header_size = (((h[3] & 0x0F) as usize) << 9) | ((h[4] as usize) << 1) | (h[5] as usize >> 7);
            if header_size < 7 || i + header_size > d.len() {
                break;
            }
            if let Some((_, mut obj)) = parse_header(&d[i..i + header_size]) {
                obj.transport_id = otid;
                entries.push(DirectoryEntry { transport_id: otid, header: obj });
            }
            i += header_size;
        }
        // an incompletely parsed directory (corrupted segments) must not replace
        // a good one and throw away the objects received so far
        if entries.len() != num_objects {
            return;
        }
        // keep objects that are still part of the new directory
        let keep: Vec<u16> = entries.iter().map(|e| e.transport_id).collect();
        self.carousel.retain(|t| keep.contains(t));
        self.directory = Some((tid, entries));
    }

    fn try_complete(&mut self, tid: u16, out: &mut Vec<MotObject>) {
        let Some(p) = self.pending.get(&tid) else { return };
        let Some(body) = p.body.complete() else { return };
        let header = if let Some(h) = &p.parsed_header {
            Some(h.clone())
        } else {
            self.directory.as_ref().and_then(|(_, e)| e.iter().find(|e| e.transport_id == tid)).map(|e| e.header.clone())
        };
        let Some(mut obj) = header else { return };
        obj.body = body;
        self.pending.remove(&tid);
        if self.recent.len() >= 8 {
            self.recent.pop_front();
        }
        self.recent.push_back((tid, obj.body_size, obj.content_name.clone()));
        self.objects_done += 1;
        self.bytes_done += obj.body.len();
        if self.directory.as_ref().map(|(_, e)| e.iter().any(|e| e.transport_id == tid)).unwrap_or(false) {
            self.carousel.insert(tid);
        }
        out.push(obj);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSC data group without CRC: type, segment number, last flag, transport id, segment data.
    fn dg(dg_type: u8, seg: u16, last: bool, tid: u16, data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x30 | dg_type, 0x00, ((last as u8) << 7) | (seg >> 8) as u8, seg as u8, 0x12, (tid >> 8) as u8, tid as u8];
        v.extend_from_slice(&[(data.len() >> 8) as u8 & 0x1F, data.len() as u8]);
        v.extend_from_slice(data);
        v
    }

    /// MOT header of a JPEG with a ContentName.
    fn header(body_size: usize, name: &str) -> Vec<u8> {
        let mut params = vec![0xCC, (name.len() + 1) as u8, 0x00];
        params.extend_from_slice(name.as_bytes());
        let hs = 7 + params.len();
        let mut h = vec![
            (body_size >> 20) as u8,
            (body_size >> 12) as u8,
            (body_size >> 4) as u8,
            ((body_size as u8 & 0x0F) << 4) | (hs >> 9) as u8,
            (hs >> 1) as u8,
            ((hs as u8 & 1) << 7) | (2 << 1), // content type 2 (image)
            0x01,                             // JFIF
        ];
        h.extend_from_slice(&params);
        h
    }

    #[test]
    fn progress_of_object_being_received() {
        let mut m = MotDecoder::new();
        let mut out = Vec::new();
        let body: Vec<u8> = (0..3000).map(|i| i as u8).collect();
        let send = |m: &mut MotDecoder, out: &mut Vec<MotObject>| {
            m.process(&dg(3, 0, true, 7, &header(3000, "a.jpg")), out);
            for (i, part) in body.chunks(1000).enumerate() {
                m.process(&dg(4, i as u16, i == 2, 7, part), out);
                if i < 2 {
                    let p = m.receiving().expect("progress");
                    assert_eq!((p.bytes, p.size, p.content_name.as_str()), ((i + 1) * 1000, 3000, "a.jpg"));
                }
            }
        };
        assert!(m.receiving().is_none());
        send(&mut m, &mut out);
        assert_eq!(out.len(), 1);
        assert!(m.receiving().is_none(), "completed object");
        // a repetition of the same object is not reported as a new slide
        m.process(&dg(3, 0, true, 7, &header(3000, "a.jpg")), &mut out);
        m.process(&dg(4, 0, false, 7, &body[..1000]), &mut out);
        assert!(m.receiving().is_none(), "repetition");
        // the next object is
        m.process(&dg(3, 0, true, 8, &header(2000, "b.jpg")), &mut out);
        m.process(&dg(4, 0, false, 8, &body[..1000]), &mut out);
        assert_eq!(m.receiving().map(|p| (p.bytes, p.size)), Some((1000, 2000)));
    }
}
