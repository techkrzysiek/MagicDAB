//! FIG parsing (EN 300 401 clause 5.2 / 6 / 8) into the [`Ensemble`] database.

use crate::charset;
use crate::ensemble::*;
use crate::puncture::{eep_bitrate, eep_code_rate, uep_info, Protection};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct ExtLabel {
    toggle: Option<bool>,
    count: u8,
    ucs2: bool,
    segments: HashMap<u8, Vec<u8>>,
}

impl ExtLabel {
    fn assemble(&self) -> Option<String> {
        if self.count == 0 || (0..self.count).any(|i| !self.segments.contains_key(&i)) {
            return None;
        }
        let mut bytes = Vec::new();
        for i in 0..self.count {
            bytes.extend_from_slice(&self.segments[&i]);
        }
        Some(charset::decode(&bytes, if self.ucs2 { 6 } else { 15 }))
    }
}

/// Pending information that refers to components by (SId, SCIdS) before FIG 0/8 arrived.
type CompKey = (u32, u8);

/// A service or sub-channel counts as no longer signalled when its FIG 0/2 or
/// FIG 0/1 entry was missing for this many of its mean repetition intervals...
const AGE_INTERVALS: f32 = 20.0;
/// ...and at least this many good FIBs (about 10 s of error-free FIC)...
const MIN_AGE: u64 = 1250;
/// ...or this many while the interval is based on few sightings.
const AGE_UNKNOWN: u64 = 3 * MIN_AGE;

/// Sightings of the FIG 0/1 or 0/2 entry of a sub-channel or service, counted in
/// good FIBs. That clock only advances with received FIC data, and the mean
/// interval between sightings measured with it does not depend on the FIB error
/// rate, so weak reception does not make entries disappear.
#[derive(Clone, Copy, Debug)]
struct Seen {
    at: u64,
    count: u32,
    interval: f32,
}

impl Seen {
    fn new(at: u64) -> Self {
        Seen { at, count: 0, interval: 0.0 }
    }

    fn saw(&mut self, at: u64) {
        if at == self.at && self.count > 0 {
            return;
        }
        if self.count > 0 {
            // running mean, later averaged over about 20 sightings
            let w = (1.0 / self.count as f32).max(0.05);
            self.interval += ((at - self.at) as f32 - self.interval) * w;
        }
        self.count += 1;
        self.at = at;
    }

    fn expired(&self, now: u64) -> bool {
        let min = if self.count > 8 { MIN_AGE } else { AGE_UNKNOWN };
        now.saturating_sub(self.at) > ((self.interval * AGE_INTERVALS) as u64).max(min)
    }
}

#[derive(Default)]
pub struct FigParser {
    pub ensemble: Ensemble,
    /// Incremented whenever the database changes.
    pub revision: u64,
    ext_labels: HashMap<(u8, u32, u8), ExtLabel>,
    pending_ua: HashMap<CompKey, Vec<UserApp>>,
    pending_labels: HashMap<CompKey, Label>,
    /// Long labels from FIG 2 (keyed like `ext_labels`).
    long_labels: HashMap<(u8, u32, u8), String>,
    pub fig_count: u64,
    /// New or changed sub-channels / new services seen once: structural changes
    /// are taken over only when received twice, so a corrupted FIB that passes
    /// the CRC (likely with weak signals) cannot create phantom entries.
    pending_subch: HashMap<u8, Subchannel>,
    pending_svc: HashMap<u32, Vec<u8>>,
    /// Good FIBs parsed: the clock for ageing out entries no longer signalled.
    fibs: u64,
    svc_seen: HashMap<u32, Seen>,
    subch_seen: HashMap<u8, Seen>,
    /// Services aged out: FIG 1 labels alone do not bring them back.
    gone: HashSet<u32>,
    /// Another EId seen once (taken over when seen again).
    pending_eid: Option<u16>,
}

fn u16be(d: &[u8], i: usize) -> u16 {
    ((d[i] as u16) << 8) | d[i + 1] as u16
}

fn u32be(d: &[u8], i: usize) -> u32 {
    ((d[i] as u32) << 24) | ((d[i + 1] as u32) << 16) | ((d[i + 2] as u32) << 8) | d[i + 3] as u32
}

impl FigParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn changed(&mut self) {
        self.revision += 1;
    }

    /// Long (FIG 2) label of the ensemble, a service or a component.
    pub fn long_label(&self, kind: u8, id: u32, scids: u8) -> Option<&String> {
        self.long_labels.get(&(kind, id, scids))
    }

    /// Remove the services and sub-channels whose FIG 0/2 / FIG 0/1 entries are
    /// no longer transmitted (e.g. a service taken off the multiplex).
    fn expire(&mut self) {
        let now = self.fibs;
        let seen = &self.svc_seen;
        let gone: Vec<u32> = self.ensemble.services.keys().copied().filter(|sid| seen.get(sid).is_some_and(|s| s.expired(now))).collect();
        for sid in gone {
            self.ensemble.services.remove(&sid);
            self.ext_labels.retain(|&(kind, id, _), _| kind == 0 || id != sid);
            self.long_labels.retain(|&(kind, id, _), _| kind == 0 || id != sid);
            self.pending_labels.retain(|&(id, _), _| id != sid);
            self.pending_ua.retain(|&(id, _), _| id != sid);
            // a stream that makes up new services all the time must not fill the memory
            if self.gone.len() >= 4096 {
                self.gone.clear();
            }
            self.gone.insert(sid);
            self.changed();
        }
        let seen = &self.subch_seen;
        let stale: Vec<u8> = self.ensemble.subchannels.keys().copied().filter(|id| seen.get(id).is_some_and(|s| s.expired(now))).collect();
        for id in stale {
            self.ensemble.subchannels.remove(&id);
            self.changed();
        }
        // forget the sightings of entries that never got confirmed
        let services = &self.ensemble.services;
        self.svc_seen.retain(|sid, s| services.contains_key(sid) || !s.expired(now));
        let subchannels = &self.ensemble.subchannels;
        self.subch_seen.retain(|id, s| subchannels.contains_key(id) || !s.expired(now));
    }

    /// Parse one FIB (30 bytes of FIG data).
    pub fn parse_fib(&mut self, fib: &[u8]) {
        self.fibs += 1;
        if self.fibs % 64 == 0 {
            self.expire();
        }
        let mut i = 0;
        while i < fib.len() {
            let hdr = fib[i];
            if hdr == 0xFF {
                break;
            }
            let ftype = hdr >> 5;
            let len = (hdr & 0x1F) as usize;
            if i + 1 + len > fib.len() || len == 0 {
                break;
            }
            let data = &fib[i + 1..i + 1 + len];
            self.fig_count += 1;
            match ftype {
                0 => self.fig0(data),
                1 => self.fig1(data),
                2 => self.fig2(data),
                _ => {}
            }
            i += 1 + len;
        }
    }

    fn fig0(&mut self, d: &[u8]) {
        if d.is_empty() {
            return;
        }
        let cn = d[0] & 0x80 != 0;
        let oe = d[0] & 0x40 != 0;
        let pd = d[0] & 0x20 != 0;
        let ext = d[0] & 0x1F;
        let body = &d[1..];
        // Only handle "current" configuration for structural FIGs.
        match ext {
            0 => self.fig0_0(body),
            1 if !cn => self.fig0_1(body),
            2 if !cn && !oe => self.fig0_2(body, pd),
            3 if !cn => self.fig0_3(body),
            5 => self.fig0_5(body),
            6 => self.fig0_6(body, cn, oe, pd),
            8 if !cn && !oe => self.fig0_8(body, pd),
            9 => self.fig0_9(body, pd),
            10 => self.fig0_10(body),
            13 if !cn && !oe => self.fig0_13(body, pd),
            14 if !cn => self.fig0_14(body),
            17 => self.fig0_17(body),
            18 if !oe => self.fig0_18(body),
            19 if !oe => self.fig0_19(body),
            21 => self.fig0_21(body, cn, oe),
            24 => self.fig0_24(body, cn, oe, pd),
            _ => {}
        }
    }

    fn fig0_0(&mut self, d: &[u8]) {
        if d.len() < 4 {
            return;
        }
        let eid = u16be(d, 0);
        let change = d[2] >> 6;
        let alarm = d[2] & 0x20 != 0;
        if self.ensemble.eid.is_some_and(|e| e != eid) {
            // another ensemble (e.g. a multiplex given a new identity): its
            // services, sub-channels and labels are not the old ones
            if self.pending_eid != Some(eid) {
                self.pending_eid = Some(eid);
                return;
            }
            let (fibs, fig_count, revision) = (self.fibs, self.fig_count, self.revision);
            *self = Self::default();
            self.fibs = fibs;
            self.fig_count = fig_count;
            self.revision = revision + 1;
        }
        self.pending_eid = None;
        let e = &mut self.ensemble;
        if e.eid != Some(eid) || e.change_flags != change || e.alarm != alarm {
            e.eid = Some(eid);
            e.change_flags = change;
            e.alarm = alarm;
            self.changed();
        }
    }

    fn fig0_1(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 3 <= d.len() {
            let id = d[i] >> 2;
            let start = (((d[i] & 0x03) as u16) << 8) | d[i + 1] as u16;
            let long = d[i + 2] & 0x80 != 0;
            let (prot, size, bitrate, label, rate) = if !long {
                let index = d[i + 2] & 0x3F;
                i += 3;
                match uep_info(index) {
                    Some((size, bitrate, level)) => (Protection::Uep { index }, size, bitrate, format!("UEP {level}"), String::new()),
                    None => continue,
                }
            } else {
                if i + 4 > d.len() {
                    break;
                }
                let option = (d[i + 2] >> 4) & 0x07;
                let level = (d[i + 2] >> 2) & 0x03;
                let size = (((d[i + 2] & 0x03) as u16) << 8) | d[i + 3] as u16;
                i += 4;
                let Some(bitrate) = eep_bitrate(option, level, size) else { continue };
                let label = format!("EEP {}-{}", level + 1, if option == 0 { 'A' } else { 'B' });
                (Protection::Eep { option, level }, size, bitrate, label, eep_code_rate(option, level).to_string())
            };
            let fec = self.ensemble.subchannels.get(&id).map(|s| s.fec_scheme).unwrap_or(0);
            let sc = Subchannel { id, start, size, protection: prot, bitrate, protection_label: label, code_rate: rate, fec_scheme: fec };
            // a CIF has 864 capacity units
            if size == 0 || start as u32 + size as u32 > 864 {
                continue;
            }
            let now = self.fibs;
            self.subch_seen.entry(id).or_insert(Seen::new(now)).saw(now);
            if self.ensemble.subchannels.get(&id) == Some(&sc) {
                self.pending_subch.remove(&id);
                continue;
            }
            if self.pending_subch.get(&id) != Some(&sc) {
                self.pending_subch.insert(id, sc);
                continue;
            }
            self.pending_subch.remove(&id);
            // sub-channels overlapping the confirmed one belong to an earlier configuration
            let (s0, s1) = (sc.start, sc.start + sc.size);
            self.ensemble.subchannels.retain(|&k, o| k == id || o.start + o.size <= s0 || o.start >= s1);
            self.ensemble.subchannels.insert(id, sc);
            self.changed();
        }
    }

    fn fig0_2(&mut self, d: &[u8], pd: bool) {
        let mut i = 0;
        let sid_len = if pd { 4 } else { 2 };
        while i + sid_len < d.len() {
            let entry_start = i;
            let sid = if pd { u32be(d, i) } else { u16be(d, i) as u32 };
            i += sid_len;
            let ca_id = (d[i] >> 4) & 0x07;
            let ncomp = (d[i] & 0x0F) as usize;
            i += 1;
            if i + 2 * ncomp > d.len() {
                break;
            }
            let mut comps = Vec::with_capacity(ncomp);
            for _ in 0..ncomp {
                let tmid = d[i] >> 6;
                let primary = d[i + 1] & 0x02 != 0;
                let ca = d[i + 1] & 0x01 != 0;
                let (ctype, subch, scid) = match tmid {
                    0 | 1 => (d[i] & 0x3F, Some(d[i + 1] >> 2), None),
                    3 => (0, None, Some((((d[i] & 0x3F) as u16) << 6) | (d[i + 1] >> 2) as u16)),
                    _ => (d[i] & 0x3F, None, None),
                };
                comps.push((tmid, ctype, subch, scid, primary, ca));
                i += 2;
            }
            let now = self.fibs;
            self.svc_seen.entry(sid).or_insert(Seen::new(now)).saw(now);
            // a new service, or other components of a known one (a reconfiguration),
            // is taken over when seen twice: a corrupted FIB that passes the CRC
            // must not change the organisation of the multiplex
            let same_set = self.ensemble.services.get(&sid).map(|s| {
                s.components.len() == comps.len()
                    && comps
                        .iter()
                        .all(|&(tmid, ctype, subch, scid, _, _)| s.components.iter().any(|c| same_component(c, tmid, ctype, subch, scid)))
            });
            if same_set != Some(true) {
                let entry = d[entry_start..i].to_vec();
                if self.pending_svc.get(&sid) != Some(&entry) {
                    if self.pending_svc.len() > 256 {
                        self.pending_svc.clear();
                    }
                    self.pending_svc.insert(sid, entry);
                    continue;
                }
                self.pending_svc.remove(&sid);
                self.gone.remove(&sid);
            }
            let svc = self.ensemble.services.entry(sid).or_insert_with(|| Service { sid, is_data: pd, ..Default::default() });
            // the entry lists every component of the service: one that is not in it
            // any more went away (e.g. the service moved to another sub-channel)
            let before = svc.components.len();
            svc.components.retain(|c| comps.iter().any(|&(tmid, ctype, subch, scid, _, _)| same_component(c, tmid, ctype, subch, scid)));
            let mut changed = svc.ca_id != ca_id || svc.components.len() != before;
            svc.ca_id = ca_id;
            for (tmid, ctype, subch, scid, primary, ca) in comps {
                let existing = svc.components.iter_mut().find(|c| same_component(c, tmid, ctype, subch, scid));
                match existing {
                    Some(c) => {
                        if c.ctype != ctype && tmid != 3 || c.primary != primary || c.ca != ca {
                            if tmid != 3 {
                                c.ctype = ctype;
                            }
                            c.primary = primary;
                            c.ca = ca;
                            changed = true;
                        }
                    }
                    None => {
                        svc.components.push(Component {
                            tmid,
                            ctype,
                            subch_id: subch,
                            scid,
                            scids: if primary { Some(0) } else { None },
                            primary,
                            ca,
                            label: None,
                            language: None,
                            user_apps: Vec::new(),
                            packet_address: None,
                            dg_flag: false,
                        });
                        changed = true;
                    }
                }
            }
            // primary component first
            svc.components.sort_by_key(|c| !c.primary);
            if changed {
                self.changed();
            }
        }
        self.apply_pending();
    }
}

/// The same service component in two FIG 0/2 entries: by sub-channel (stream
/// modes), by SCId (packet mode) or by FIDCId (TMId 2 of older multiplexes).
fn same_component(c: &Component, tmid: u8, ctype: u8, subch: Option<u8>, scid: Option<u16>) -> bool {
    c.tmid == tmid
        && match tmid {
            0 | 1 => c.subch_id == subch,
            3 => c.scid == scid,
            _ => c.ctype == ctype,
        }
}

impl FigParser {
    fn fig0_3(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 5 <= d.len() {
            let scid = (u16be(d, i) >> 4) & 0x0FFF;
            let caorg = d[i + 1] & 0x01 != 0;
            let dg_flag = d[i + 2] & 0x80 != 0;
            let dscty = d[i + 2] & 0x3F;
            let subch = d[i + 3] >> 2;
            let addr = u16be(d, i + 3) & 0x03FF;
            i += 5 + if caorg { 2 } else { 0 };
            let mut changed = false;
            for svc in self.ensemble.services.values_mut() {
                for c in svc.components.iter_mut().filter(|c| c.scid == Some(scid)) {
                    if c.subch_id != Some(subch) || c.packet_address != Some(addr) || c.ctype != dscty || c.dg_flag != !dg_flag {
                        c.subch_id = Some(subch);
                        c.packet_address = Some(addr);
                        c.ctype = dscty;
                        // DG flag 0 means data groups are used
                        c.dg_flag = !dg_flag;
                        changed = true;
                    }
                }
            }
            if changed {
                self.changed();
            }
        }
    }

    fn fig0_5(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 2 <= d.len() {
            let long = d[i] & 0x80 != 0;
            if !long {
                let fidc = d[i] & 0x40 != 0;
                let subch = d[i] & 0x3F;
                let lang = d[i + 1];
                i += 2;
                if fidc {
                    continue;
                }
                let mut changed = false;
                for svc in self.ensemble.services.values_mut() {
                    for c in svc.components.iter_mut().filter(|c| c.subch_id == Some(subch) && c.tmid != 3) {
                        if c.language != Some(lang) {
                            c.language = Some(lang);
                            changed = true;
                        }
                    }
                    if svc.primary().map(|c| c.subch_id == Some(subch) && c.tmid != 3).unwrap_or(false) {
                        svc.language = Some(lang);
                    }
                }
                if changed {
                    self.changed();
                }
            } else {
                if i + 3 > d.len() {
                    break;
                }
                let scid = u16be(d, i) & 0x0FFF;
                let lang = d[i + 2];
                i += 3;
                let mut changed = false;
                for svc in self.ensemble.services.values_mut() {
                    for c in svc.components.iter_mut().filter(|c| c.scid == Some(scid)) {
                        if c.language != Some(lang) {
                            c.language = Some(lang);
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.changed();
                }
            }
        }
    }

    fn fig0_6(&mut self, d: &[u8], cn: bool, oe: bool, pd: bool) {
        let mut i = 0;
        while i + 2 <= d.len() {
            let id_list = d[i] & 0x80 != 0;
            let la = d[i] & 0x40 != 0;
            let sh = d[i] & 0x20 != 0;
            let ils = d[i] & 0x10 != 0;
            let lsn = u16be(d, i) & 0x0FFF;
            i += 2;
            let mut ids = Vec::new();
            let mut key_sid = None;
            if id_list {
                if i >= d.len() {
                    break;
                }
                let n = (d[i] & 0x0F) as usize;
                let idlq = (d[i] >> 5) & 0x03;
                i += 1;
                for j in 0..n {
                    let (ecc, id, step) = if pd {
                        if i + 4 > d.len() {
                            break;
                        }
                        (None, u32be(d, i), 4)
                    } else if ils {
                        if i + 3 > d.len() {
                            break;
                        }
                        (Some(d[i]), u16be(d, i + 1) as u32, 3)
                    } else {
                        if i + 2 > d.len() {
                            break;
                        }
                        (None, u16be(d, i) as u32, 2)
                    };
                    i += step;
                    if j == 0 && !oe && !cn {
                        key_sid = Some(id);
                        continue;
                    }
                    ids.push(LinkedId { kind: if pd { 0 } else { idlq }, ecc, id });
                }
            }
            let list = &mut self.ensemble.linkage;
            let pos = list.iter().position(|l| l.lsn == lsn && l.oe == oe && l.hard == sh && l.international == ils);
            let mut changed = false;
            match pos {
                Some(p) => {
                    let l = &mut list[p];
                    if l.active != la {
                        l.active = la;
                        changed = true;
                    }
                    if key_sid.is_some() && l.key_sid != key_sid {
                        l.key_sid = key_sid;
                        changed = true;
                    }
                    for id in ids {
                        if !l.ids.contains(&id) {
                            l.ids.push(id);
                            changed = true;
                        }
                    }
                }
                None => {
                    if id_list {
                        list.push(LinkageSet { lsn, active: la, hard: sh, international: ils, oe, key_sid, ids });
                        changed = true;
                    }
                }
            }
            if changed {
                self.changed();
            }
        }
    }

    fn fig0_8(&mut self, d: &[u8], pd: bool) {
        let mut i = 0;
        let sid_len = if pd { 4 } else { 2 };
        while i + sid_len + 2 <= d.len() {
            let sid = if pd { u32be(d, i) } else { u16be(d, i) as u32 };
            i += sid_len;
            let ext = d[i] & 0x80 != 0;
            let scids = d[i] & 0x0F;
            i += 1;
            let long = d[i] & 0x80 != 0;
            let (subch, scid) = if long {
                if i + 2 > d.len() {
                    break;
                }
                let v = u16be(d, i) & 0x0FFF;
                i += 2;
                (None, Some(v))
            } else {
                let v = d[i] & 0x3F;
                i += 1;
                (Some(v), None)
            };
            if ext {
                i += 1;
            }
            let mut changed = false;
            if let Some(svc) = self.ensemble.services.get_mut(&sid) {
                for c in svc.components.iter_mut() {
                    let matches = match (subch, scid) {
                        (Some(s), _) => c.subch_id == Some(s) && c.tmid != 3,
                        (_, Some(id)) => c.scid == Some(id),
                        _ => false,
                    };
                    if matches && c.scids != Some(scids) {
                        c.scids = Some(scids);
                        changed = true;
                    }
                }
            }
            if changed {
                self.changed();
                self.apply_pending();
            }
        }
    }

    fn fig0_9(&mut self, d: &[u8], pd: bool) {
        if d.len() < 3 {
            return;
        }
        let ext = d[0] & 0x80 != 0;
        let mut lto = (d[0] & 0x3F) as i8;
        if lto & 0x20 != 0 {
            lto = -(lto & 0x1F);
        }
        let ecc = d[1];
        let table = d[2];
        let e = &mut self.ensemble;
        let mut changed = false;
        if e.lto != Some(lto) || e.ecc != Some(ecc) || e.inter_table_id != Some(table) {
            e.lto = Some(lto);
            e.ecc = Some(ecc);
            e.inter_table_id = Some(table);
            changed = true;
        }
        if ext {
            let mut i = 3;
            while i < d.len() {
                let n = (d[i] >> 6) as usize;
                i += 1;
                if pd {
                    // 32 bit SIds carry their own ECC
                    i += 4 * n;
                    continue;
                }
                if i >= d.len() {
                    break;
                }
                let secc = d[i];
                i += 1;
                for _ in 0..n {
                    if i + 2 > d.len() {
                        break;
                    }
                    let sid = u16be(d, i) as u32;
                    i += 2;
                    if let Some(s) = e.services.get_mut(&sid) {
                        if s.ecc != Some(secc) {
                            s.ecc = Some(secc);
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            self.changed();
        }
    }

    fn fig0_10(&mut self, d: &[u8]) {
        if d.len() < 4 {
            return;
        }
        let mjd = (((d[0] & 0x7F) as u32) << 10) | ((d[1] as u32) << 2) | (d[2] >> 6) as u32;
        let lsi = d[2] & 0x20 != 0;
        let utc_long = d[2] & 0x08 != 0;
        let hours = ((d[2] & 0x07) << 2) | (d[3] >> 6);
        let minutes = d[3] & 0x3F;
        let (seconds, millis) = if utc_long && d.len() >= 6 { (d[4] >> 2, (((d[4] & 0x03) as u16) << 8) | d[5] as u16) } else { (0, 0) };
        let t = DabTime { mjd, hours, minutes, seconds, millis, lsi };
        // time is not part of the revision counter (changes every minute)
        self.ensemble.time = Some(t);
    }

    fn fig0_13(&mut self, d: &[u8], pd: bool) {
        let mut i = 0;
        let sid_len = if pd { 4 } else { 2 };
        while i + sid_len < d.len() {
            let sid = if pd { u32be(d, i) } else { u16be(d, i) as u32 };
            i += sid_len;
            let scids = d[i] >> 4;
            let n = (d[i] & 0x0F) as usize;
            i += 1;
            let mut uas = Vec::new();
            for _ in 0..n {
                if i + 2 > d.len() {
                    break;
                }
                let ua_type = u16be(d, i) >> 5;
                let len = (d[i + 1] & 0x1F) as usize;
                i += 2;
                if i + len > d.len() {
                    break;
                }
                let data = d[i..i + len].to_vec();
                i += len;
                let (xpad, dscty, dg) = if !pd && len >= 2 {
                    let ca = data[0] & 0x80 != 0;
                    let _ = ca;
                    (Some(data[0] & 0x1F), Some(data[1] & 0x3F), data[1] & 0x80 == 0)
                } else {
                    (None, None, false)
                };
                uas.push(UserApp { ua_type, xpad_app_type: xpad, dscty, dg_flag: dg, data });
            }
            self.pending_ua.insert((sid, scids), uas);
        }
        self.apply_pending();
    }

    fn fig0_14(&mut self, d: &[u8]) {
        for &b in d {
            let id = b >> 2;
            let fec = b & 0x03;
            if let Some(s) = self.ensemble.subchannels.get_mut(&id) {
                if s.fec_scheme != fec {
                    s.fec_scheme = fec;
                    self.revision += 1;
                }
            }
        }
    }

    fn fig0_17(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 4 <= d.len() {
            let sid = u16be(d, i) as u32;
            let sd = d[i + 2] & 0x80 != 0;
            let l_flag = d[i + 2] & 0x20 != 0;
            let cc_flag = d[i + 2] & 0x10 != 0;
            i += 3;
            if l_flag {
                i += 1;
            }
            if i >= d.len() {
                break;
            }
            let pty = d[i] & 0x1F;
            i += 1;
            if cc_flag {
                i += 1;
            }
            if let Some(s) = self.ensemble.services.get_mut(&sid) {
                if s.pty != Some(pty) || s.pty_dynamic != sd {
                    s.pty = Some(pty);
                    s.pty_dynamic = sd;
                    self.revision += 1;
                }
            }
        }
    }

    fn fig0_18(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 5 <= d.len() {
            let sid = u16be(d, i) as u32;
            let asu = u16be(d, i + 2);
            let n = (d[i + 4] & 0x1F) as usize;
            i += 5;
            if i + n > d.len() {
                break;
            }
            let clusters = d[i..i + n].to_vec();
            i += n;
            if let Some(s) = self.ensemble.services.get_mut(&sid) {
                if s.asu != asu || s.clusters != clusters {
                    s.asu = asu;
                    s.clusters = clusters;
                    self.revision += 1;
                }
            }
        }
    }

    fn fig0_19(&mut self, d: &[u8]) {
        let mut i = 0;
        while i + 4 <= d.len() {
            let cluster = d[i];
            let asw = u16be(d, i + 1);
            let new_flag = d[i + 3] & 0x80 != 0;
            let region_flag = d[i + 3] & 0x40 != 0;
            let subch_id = d[i + 3] & 0x3F;
            i += 4;
            let region = if region_flag && i < d.len() {
                let r = d[i] & 0x3F;
                i += 1;
                Some(r)
            } else {
                None
            };
            let a = AnnouncementSwitch { cluster, asw, new_flag, subch_id, region };
            let anns = &mut self.ensemble.announcements;
            if asw == 0 {
                if anns.remove(&cluster).is_some() {
                    self.revision += 1;
                }
            } else if anns.get(&cluster) != Some(&a) {
                anns.insert(cluster, a);
                self.revision += 1;
            }
        }
    }

    fn fig0_21(&mut self, d: &[u8], _cn: bool, oe: bool) {
        let mut i = 0;
        while i + 2 <= d.len() {
            let region_id = ((d[i] as u16) << 3) | (d[i + 1] >> 5) as u16;
            let len = (d[i + 1] & 0x1F) as usize;
            i += 2;
            let end = (i + len).min(d.len());
            while i + 3 <= end {
                let mut id = u16be(d, i) as u32;
                let rm = d[i + 2] >> 4;
                let continuity = d[i + 2] & 0x08 != 0;
                let flen = (d[i + 2] & 0x07) as usize;
                i += 3;
                let fend = (i + flen).min(end);
                let mut freqs = Vec::new();
                match rm {
                    0 | 1 => {
                        while i + 3 <= fend {
                            let ctrl = d[i] >> 3;
                            let f = ((((d[i] & 0x07) as u32) << 16) | ((d[i + 1] as u32) << 8) | d[i + 2] as u32) * 16;
                            i += 3;
                            if f != 0 {
                                freqs.push(FrequencyEntry { khz: f, adjacent: Some(ctrl & 0x01 == 0), mode: Some((ctrl >> 1) & 0x07) });
                            }
                        }
                    }
                    8 | 9 => {
                        while i < fend {
                            let code = d[i] as u32;
                            i += 1;
                            if code != 0 && code < 205 {
                                freqs.push(FrequencyEntry { khz: 87_500 + code * 100, adjacent: None, mode: None });
                            }
                        }
                    }
                    10 => {
                        while i < fend {
                            let code = d[i] as u32;
                            i += 1;
                            if code != 0 {
                                let khz = if code < 16 { 144 + code * 9 } else { 387 + code * 9 };
                                freqs.push(FrequencyEntry { khz, adjacent: None, mode: None });
                            }
                        }
                    }
                    12 => {
                        while i + 2 <= fend {
                            let khz = u16be(d, i) as u32 * 5;
                            i += 2;
                            if khz != 0 {
                                freqs.push(FrequencyEntry { khz, adjacent: None, mode: None });
                            }
                        }
                    }
                    6 | 14 => {
                        if i < fend {
                            id |= (d[i] as u32) << 16;
                            i += 1;
                        }
                        while i + 2 <= fend {
                            let v = u16be(d, i);
                            i += 2;
                            let khz = if rm == 6 && v & 0x8000 != 0 { (v & 0x7FFF) as u32 * 10 } else { (v & 0x7FFF) as u32 };
                            if khz != 0 {
                                freqs.push(FrequencyEntry { khz, adjacent: None, mode: None });
                            }
                        }
                    }
                    _ => {}
                }
                i = fend;
                let list = &mut self.ensemble.frequency_info;
                let pos = list.iter().position(|f| f.id == id && f.rm == rm && f.oe == oe && f.region_id == region_id);
                let mut changed = false;
                match pos {
                    Some(p) => {
                        let fi = &mut list[p];
                        if fi.continuity != continuity {
                            fi.continuity = continuity;
                            changed = true;
                        }
                        for f in freqs {
                            if !fi.frequencies.iter().any(|x| x.khz == f.khz) {
                                fi.frequencies.push(f);
                                changed = true;
                            }
                        }
                        fi.frequencies.sort_by_key(|f| f.khz);
                    }
                    None => {
                        let mut freqs = freqs;
                        freqs.sort_by_key(|f| f.khz);
                        list.push(FrequencyInfo { region_id, oe, id, rm, continuity, frequencies: freqs });
                        changed = true;
                    }
                }
                if changed {
                    self.revision += 1;
                }
            }
            i = end;
        }
    }

    fn fig0_24(&mut self, d: &[u8], cn: bool, oe: bool, pd: bool) {
        let _ = (cn, oe);
        let mut i = 0;
        let sid_len = if pd { 4 } else { 2 };
        while i + sid_len < d.len() {
            let sid = if pd { u32be(d, i) } else { u16be(d, i) as u32 };
            i += sid_len;
            let ca_id = (d[i] >> 4) & 0x07;
            let n = (d[i] & 0x0F) as usize;
            i += 1;
            let mut eids = Vec::new();
            for _ in 0..n {
                if i + 2 > d.len() {
                    break;
                }
                eids.push(u16be(d, i));
                i += 2;
            }
            let list = &mut self.ensemble.oe_services;
            let mut changed = false;
            match list.iter_mut().find(|o| o.sid == sid) {
                Some(o) => {
                    for e in eids {
                        if !o.eids.contains(&e) {
                            o.eids.push(e);
                            changed = true;
                        }
                    }
                }
                None => {
                    list.push(OeService { sid, ca_id, eids });
                    changed = true;
                }
            }
            if changed {
                self.revision += 1;
            }
        }
    }

    fn fig1(&mut self, d: &[u8]) {
        if d.is_empty() {
            return;
        }
        let charset = d[0] >> 4;
        let oe = d[0] & 0x08 != 0;
        let ext = d[0] & 0x07;
        if oe {
            return;
        }
        let body = &d[1..];
        let label_at = |off: usize| -> Option<Label> {
            if body.len() < off + 18 {
                return None;
            }
            let text = charset::decode(&body[off..off + 16], charset);
            let flags = u16be(body, off + 16);
            Some(Label::new(text, flags))
        };
        match ext {
            0 => {
                if let Some(l) = label_at(2) {
                    if self.ensemble.label.as_ref() != Some(&l) {
                        self.ensemble.label = Some(l);
                        self.changed();
                    }
                }
            }
            1 => {
                if body.len() < 2 {
                    return;
                }
                let sid = u16be(body, 0) as u32;
                if let Some(l) = label_at(2) {
                    if self.gone.contains(&sid) {
                        return;
                    }
                    self.svc_seen.entry(sid).or_insert(Seen::new(self.fibs));
                    let svc = self.ensemble.services.entry(sid).or_insert_with(|| Service { sid, ..Default::default() });
                    if svc.label.as_ref() != Some(&l) {
                        svc.label = Some(l);
                        self.changed();
                    }
                }
            }
            4 => {
                if body.is_empty() {
                    return;
                }
                let pd = body[0] & 0x80 != 0;
                let scids = body[0] & 0x0F;
                let (sid, off) = if pd {
                    if body.len() < 5 {
                        return;
                    }
                    (u32be(body, 1), 5)
                } else {
                    if body.len() < 3 {
                        return;
                    }
                    (u16be(body, 1) as u32, 3)
                };
                if let Some(l) = label_at(off) {
                    self.pending_labels.insert((sid, scids), l);
                    self.apply_pending();
                }
            }
            5 => {
                if body.len() < 4 {
                    return;
                }
                let sid = u32be(body, 0);
                if let Some(l) = label_at(4) {
                    if self.gone.contains(&sid) {
                        return;
                    }
                    self.svc_seen.entry(sid).or_insert(Seen::new(self.fibs));
                    let svc = self.ensemble.services.entry(sid).or_insert_with(|| Service { sid, is_data: true, ..Default::default() });
                    if svc.label.as_ref() != Some(&l) {
                        svc.label = Some(l);
                        self.changed();
                    }
                }
            }
            _ => {}
        }
    }

    fn fig2(&mut self, d: &[u8]) {
        if d.is_empty() {
            return;
        }
        let toggle = d[0] & 0x80 != 0;
        let seg = (d[0] >> 4) & 0x07;
        let rfu = d[0] & 0x08 != 0;
        let ext = d[0] & 0x07;
        let body = &d[1..];
        let (id, scids, idlen) = match ext {
            0 | 1 => {
                if body.len() < 2 {
                    return;
                }
                (u16be(body, 0) as u32, 0u8, 2)
            }
            4 => {
                if body.is_empty() {
                    return;
                }
                let pd = body[0] & 0x80 != 0;
                let scids = body[0] & 0x0F;
                if pd {
                    if body.len() < 5 {
                        return;
                    }
                    (u32be(body, 1), scids, 5)
                } else {
                    if body.len() < 3 {
                        return;
                    }
                    (u16be(body, 1) as u32, scids, 3)
                }
            }
            5 => {
                if body.len() < 4 {
                    return;
                }
                (u32be(body, 0), 0, 4)
            }
            _ => return,
        };
        let mut f = &body[idlen..];
        let key = (ext, id, scids);
        let entry = self.ext_labels.entry(key).or_default();
        if entry.toggle != Some(toggle) {
            entry.segments.clear();
            entry.count = 0;
            entry.toggle = Some(toggle);
        }
        if seg == 0 {
            if f.is_empty() {
                return;
            }
            entry.ucs2 = f[0] & 0x80 != 0;
            entry.count = ((f[0] >> 4) & 0x07) + 1;
            let skip = if rfu { 1 } else { 3 };
            if f.len() < skip {
                return;
            }
            f = &f[skip..];
        }
        entry.segments.insert(seg, f.to_vec());
        if let Some(text) = entry.assemble() {
            let text = text.trim().to_string();
            if self.long_labels.get(&key) != Some(&text) {
                self.long_labels.insert(key, text);
                self.revision += 1;
            }
        }
    }

    /// Attach user applications and component labels waiting for FIG 0/2 / 0/8.
    fn apply_pending(&mut self) {
        let mut changed = false;
        let ens = &mut self.ensemble;
        for (&(sid, scids), uas) in self.pending_ua.iter() {
            if let Some(svc) = ens.services.get_mut(&sid) {
                let pos = svc.components.iter().position(|c| c.scids == Some(scids)).or_else(|| {
                    if scids == 0 {
                        svc.components.iter().position(|c| c.primary)
                    } else {
                        None
                    }
                });
                if let Some(p) = pos {
                    let c = &mut svc.components[p];
                    let mut uas = uas.clone();
                    if !c.is_audio() {
                        // X-PAD fields only exist for applications in the PAD of an audio component
                        for ua in uas.iter_mut() {
                            ua.xpad_app_type = None;
                            ua.dscty = None;
                            ua.dg_flag = false;
                        }
                    }
                    if c.user_apps != uas {
                        c.user_apps = uas;
                        changed = true;
                    }
                }
            }
        }
        for (&(sid, scids), label) in self.pending_labels.iter() {
            if let Some(svc) = ens.services.get_mut(&sid) {
                let comp = match svc.components.iter_mut().position(|c| c.scids == Some(scids)) {
                    Some(p) => Some(&mut svc.components[p]),
                    None if scids == 0 => svc.components.iter_mut().find(|c| c.primary),
                    None => None,
                };
                if let Some(c) = comp {
                    if c.label.as_ref() != Some(label) {
                        c.label = Some(label.clone());
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fib(figs: &[Vec<u8>]) -> Vec<u8> {
        let mut f = figs.concat();
        assert!(f.len() <= 30);
        f.resize(30, 0xFF);
        f
    }

    fn fig0_0() -> Vec<u8> {
        vec![0x05, 0x00, 0x4D, 0x62, 0x00, 0x00]
    }

    /// FIG 0/2 with one DAB+ audio service on `subch`.
    fn fig0_2(sid: u16, subch: u8) -> Vec<u8> {
        vec![0x06, 0x02, (sid >> 8) as u8, sid as u8, 0x01, 0x3F, (subch << 2) | 0x02]
    }

    fn fig1_1(sid: u16, text: &str) -> Vec<u8> {
        let mut v = vec![0x20 | 21, 0x01, (sid >> 8) as u8, sid as u8];
        let mut t = text.as_bytes().to_vec();
        t.resize(16, b' ');
        v.extend_from_slice(&t);
        v.extend_from_slice(&[0xFF, 0x00]);
        v
    }

    #[test]
    fn service_moved_to_another_subchannel() {
        let mut p = FigParser::new();
        let old = fib(&[fig0_0(), fig0_2(0x1111, 1)]);
        for _ in 0..3 {
            p.parse_fib(&old);
        }
        let new = fib(&[fig0_0(), fig0_2(0x1111, 9)]);
        p.parse_fib(&new);
        let subch = |p: &FigParser| p.ensemble.services[&0x1111].components.iter().map(|c| c.subch_id).collect::<Vec<_>>();
        assert_eq!(subch(&p), vec![Some(1)], "once is not enough");
        p.parse_fib(&new);
        assert_eq!(subch(&p), vec![Some(9)]);
        assert_eq!(p.ensemble.services[&0x1111].primary().and_then(|c| c.subch_id), Some(9));
        // repetitions change nothing
        let rev = p.revision;
        p.parse_fib(&new);
        assert_eq!(p.revision, rev);
    }

    #[test]
    fn new_eid_starts_a_new_database() {
        let mut p = FigParser::new();
        let old = fib(&[fig0_0(), fig0_2(0x1111, 1)]);
        for _ in 0..3 {
            p.parse_fib(&old);
        }
        assert!(p.ensemble.services.contains_key(&0x1111));
        // FIG 0/0 with EId C1A5 twice, then the new service
        let new_eid = vec![0x05, 0x00, 0xC1, 0xA5, 0x00, 0x00];
        p.parse_fib(&fib(std::slice::from_ref(&new_eid)));
        assert_eq!(p.ensemble.eid, Some(0x4D62), "once is not enough");
        p.parse_fib(&fib(std::slice::from_ref(&new_eid)));
        assert_eq!(p.ensemble.eid, Some(0xC1A5));
        assert!(p.ensemble.services.is_empty());
        for _ in 0..2 {
            p.parse_fib(&fib(&[new_eid.clone(), fig0_2(0xD4A2, 1)]));
        }
        assert!(p.ensemble.services.contains_key(&0xD4A2) && !p.ensemble.services.contains_key(&0x1111));
    }

    #[test]
    fn removed_service_ages_out() {
        let mut p = FigParser::new();
        let both = fib(&[fig0_0(), fig0_2(0x1111, 1), fig0_2(0x2222, 2)]);
        let one = fib(&[fig0_0(), fig0_2(0x1111, 1)]);
        let filler = fib(&[fig0_0()]);
        for i in 0..5000 {
            p.parse_fib(if i % 10 == 0 { &both } else { &filler });
        }
        assert!(p.ensemble.services.contains_key(&0x2222));
        let rev = p.revision;
        let mut removed = None;
        for i in 0..5000 {
            p.parse_fib(if i % 10 == 0 { &one } else { &filler });
            if removed.is_none() && !p.ensemble.services.contains_key(&0x2222) {
                removed = Some(i);
            }
        }
        // the FIG 0/2 cycle is 10 FIBs: the 10 s minimum applies
        let n = removed.expect("service not removed") as u64;
        assert!(n + 10 > MIN_AGE && n < MIN_AGE + 80, "removed after {n} FIBs");
        assert!(p.ensemble.services.contains_key(&0x1111));
        assert!(p.revision > rev);
    }

    #[test]
    fn slow_cycle_ages_out_after_intervals() {
        let mut p = FigParser::new();
        let a = fib(&[fig0_0(), fig0_2(0x1111, 1)]);
        let b = fib(&[fig0_0(), fig0_2(0x2222, 2)]);
        let filler = fib(&[fig0_0()]);
        for i in 0..50_000 {
            p.parse_fib(match i % 1000 {
                0 => &a,
                500 => &b,
                _ => &filler,
            });
        }
        assert!(p.ensemble.services.contains_key(&0x2222));
        let mut removed = None;
        for i in 0..30_000 {
            p.parse_fib(if i % 1000 == 0 { &a } else { &filler });
            if removed.is_none() && !p.ensemble.services.contains_key(&0x2222) {
                removed = Some(i);
            }
        }
        // last sighting 500 FIBs before; 20 intervals of 1000 FIBs
        let n = removed.expect("service not removed") + 500;
        assert!((20_000..20_100).contains(&n), "removed {n} FIBs after the last sighting");
    }

    #[test]
    fn weak_reception_keeps_services() {
        let mut p = FigParser::new();
        let a = fib(&[fig0_0(), fig0_2(0x1111, 1)]);
        let b = fib(&[fig0_0(), fig0_2(0x2222, 2)]);
        let filler = fib(&[fig0_0()]);
        let mut rng = 12345u64;
        let mut confirmed = false;
        for i in 0..3_000_000u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // 90 % of the FIBs fail the CRC
            if (rng >> 33) % 10 != 0 {
                continue;
            }
            p.parse_fib(match i % 100 {
                0 => &a,
                50 => &b,
                _ => &filler,
            });
            let both = p.ensemble.services.contains_key(&0x1111) && p.ensemble.services.contains_key(&0x2222);
            if both {
                confirmed = true;
            } else {
                assert!(!confirmed, "service lost after {i} FIBs");
            }
        }
        assert!(confirmed);
    }

    #[test]
    fn labels_do_not_resurrect_removed_service() {
        let mut p = FigParser::new();
        let label = fib(&[fig1_1(0x3333, "Gone"), fig0_2(0x1111, 1)]);
        let svc = fib(&[fig0_0(), fig0_2(0x3333, 3)]);
        // a label without FIG 0/2 creates the service for a while only
        for _ in 0..(AGE_UNKNOWN + 200) {
            p.parse_fib(&label);
        }
        assert!(!p.ensemble.services.contains_key(&0x3333));
        assert!(p.ensemble.services.contains_key(&0x1111));
        // back when FIG 0/2 confirms it again
        p.parse_fib(&svc);
        assert!(!p.ensemble.services.contains_key(&0x3333));
        p.parse_fib(&svc);
        assert!(p.ensemble.services.contains_key(&0x3333));
        p.parse_fib(&label);
        assert_eq!(p.ensemble.services[&0x3333].label.as_ref().map(|l| l.text.trim()), Some("Gone"));
    }
}
