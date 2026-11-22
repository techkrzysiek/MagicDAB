//! TPEG over DAB (TDC data service, TS 101 759; TPEG2, ISO 21219): transport
//! frames, Service and Network Information (SNI) and Traffic Event Compact
//! (TEC) messages, kept by message ID with updates, cancellations and expiry.
//! Other applications (TFP, PKI, EAW ...) are counted and skipped.
//!
//! - Transport frame: sync word 0xFF0F, field length, header CRC (over the
//!   sync word, field length, frame type and the first 11 bytes of the
//!   service frame), frame type: 0 stream directory, 1 service frame.
//! - Service frame: SID A/B/C, encryption indicator (0 none, 107 zlib), then
//!   service component frames: SCID, field length, header CRC (over SCID,
//!   field length and the first 13 data bytes), data.
//! - SNI (SCID 0) tells the application (AID) of every other SCID. TEC data
//!   is group priority, message count, messages and a data CRC.
//!
//! The decoder takes the bytes of the stream in any chunks (`push`) or the
//! MSC data groups of a packet mode component (`push_data_group`); transport
//! frames may span data groups. It reads no clock: `expire` drops messages
//! whose expiry time has passed.

mod binary;
mod lrc;
mod tables;
mod tec;

use crate::crc::{check_crc16, crc16_ccitt};
use binary::Reader;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A code of a TPEG table with its English name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Code {
    pub code: u8,
    pub name: &'static str,
}

/// Cause of a traffic event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cause {
    /// Main cause (tec002).
    pub code: u8,
    pub name: &'static str,
    /// Sub-cause (tec101 to tec131, by main cause).
    pub sub_code: Option<u8>,
    pub sub_name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<Code>,
    #[serde(skip_serializing_if = "is_false")]
    pub unverified: bool,
    /// Metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length_affected: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_restriction: Option<Code>,
    /// Number of lanes the restriction applies to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lanes: Option<u8>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
    /// Metres from the start of the location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// The message ID of the event causing this one (a linked cause).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_message: Option<u32>,
}

/// Advice to drivers (tec005, sub-advice tec202 to tec216).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    pub code: Option<u8>,
    pub name: Option<&'static str>,
    pub sub_code: Option<u8>,
    pub sub_name: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
}

/// Vehicles an event applies to (tec009) and conditions (tec007).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VehicleRestriction {
    pub vehicle: Option<Code>,
    pub conditions: Vec<Condition>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Condition {
    pub code: u8,
    pub name: &'static str,
    /// Centimetres, kilograms or a count, by condition.
    pub value: Option<u32>,
}

/// Temporary speed limit section.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpeedLimit {
    /// km/h, or mph with `mph`.
    pub value: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wet: Option<u8>,
    /// Metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    pub mph: bool,
}

/// Where an event is.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// The method the points come from ("OLR", "GLR"), without points the
    /// first method in the container ("TMC", "ETL", "#<id>" when unknown).
    pub method: String,
    /// Every method in the container.
    pub methods: Vec<String>,
    /// "line", "pointAlongLine", "poiWithAccessPoint", "point", "circle",
    /// "polygon", "rectangle", "grid", "closedLine", "boundingBox", "area".
    pub kind: Option<&'static str>,
    /// [latitude, longitude]: the OLR location reference points in order
    /// (first, intermediate, last), or the GLR points.
    pub points: Vec<[f64; 2]>,
    /// The point of a point location; along a line approximated between the
    /// first two location reference points.
    pub point: Option<[f64; 2]>,
    /// The OLR shape (the road geometry), when sent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shape: Vec<[f64; 2]>,
    /// Metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<u32>,
    /// Metres from the first point to the start, from the last one to the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positive_offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_offset: Option<u32>,
    /// olr004: 0 on or unknown, 1 right, 2 left, 3 both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_of_road: Option<u8>,
    /// olr003: 0 unknown, 1 with, 2 against the line direction, 3 both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u8>,
    pub tmc: Option<TmcLocation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
}

/// A TMC location (ALERT-C location table code).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmcLocation {
    pub location: u16,
    pub country: u8,
    pub table: u8,
    pub ecc: Option<u8>,
    /// Direction positive: from the primary location along the location list.
    pub positive: bool,
    pub both_directions: bool,
    pub extent: u8,
    /// Location table version, "major.minor".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_version: Option<String>,
    /// Metres upstream of the primary location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<u32>,
    /// Metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(skip_serializing_if = "is_false")]
    pub internal_primary: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub internal_secondary: bool,
}

/// A traffic message.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEvent {
    /// "A.B.C/scid/id", see `EventKey`.
    pub key: String,
    /// TPEG service ID, A.B.C.
    pub sid: String,
    /// Service component the message came in.
    pub scid: u8,
    /// Message ID (MMC).
    pub id: u32,
    pub version: u8,
    pub application: &'static str,
    /// Seconds since 1970-01-01 UTC.
    pub expires: u64,
    pub generated: Option<u64>,
    pub priority: Option<u8>,
    /// Effect on the traffic flow (tec001).
    pub effect: Option<Code>,
    pub causes: Vec<Cause>,
    /// The highest warning level of the causes (tec003).
    pub warning: Option<Code>,
    pub location: Option<Location>,
    /// Seconds since 1970-01-01 UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tendency: Option<Code>,
    /// Metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length_affected: Option<u32>,
    /// m/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_speed: Option<u8>,
    /// Minutes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<u32>,
    /// m/s, for routing only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_limit: Option<u8>,
    /// m/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_speed: Option<u8>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub advice: Vec<Advice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<VehicleRestriction>,
    #[serde(skip_serializing_if = "is_zero")]
    pub diversion_routes: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub speed_limits: Vec<SpeedLimit>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Identifies a message: TPEG service, service component, message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey {
    pub sid: u32,
    pub scid: u8,
    pub id: u32,
}

impl fmt::Display for EventKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}/{}", sid_text(self.sid), self.scid, self.id)
    }
}

impl Serialize for EventKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

fn sid_text(sid: u32) -> String {
    format!("{}.{}.{}", sid >> 16, (sid >> 8) & 0xFF, sid & 0xFF)
}

/// A TPEG service as its SNI describes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TpegService {
    pub sid: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub components: Vec<TpegComponent>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TpegComponent {
    pub scid: u8,
    pub aid: u16,
    /// "SNI", "TEC", "TFP" ..., "unknown".
    pub application: &'static str,
    /// Specification version, "major.minor".
    pub version: Option<String>,
    /// Content description.
    pub content: Option<String>,
    pub encrypted: bool,
}

/// Frames and messages per application.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AppStats {
    pub aid: u16,
    pub name: &'static str,
    /// Service component frames.
    pub frames: u64,
    /// Messages in them (repetitions included); not counted for frames of
    /// an unknown structure.
    pub messages: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TpegStats {
    pub data_groups: u64,
    /// Data groups with a wrong CRC or header.
    pub data_group_errors: u64,
    /// Bytes skipped looking for a transport frame.
    pub skipped_bytes: u64,
    /// Transport frames with a good header CRC.
    pub frames: u64,
    pub stream_directories: u64,
    /// Transport frames where one was due, and service component frames,
    /// with a wrong header CRC.
    pub header_crc_errors: u64,
    /// Data CRC errors (SNI, TEC, stream directory).
    pub data_crc_errors: u64,
    /// Service and component frames left encrypted.
    pub encrypted: u64,
    pub inflate_errors: u64,
    /// Component frames before the SNI told their application.
    pub unknown_components: u64,
    /// Messages that could not be decoded, wrong message counts.
    pub message_errors: u64,
    pub cancellations: u64,
    pub expired: u64,
    /// By application ID.
    pub apps: Vec<AppStats>,
}

#[derive(Default)]
struct ServiceState {
    name: Option<String>,
    description: Option<String>,
    /// SCID -> (AID, encrypted), from the fast tuning table.
    apps: BTreeMap<u8, (u16, bool)>,
    versions: BTreeMap<u8, (u8, u8)>,
    contents: BTreeMap<u8, String>,
}

/// zlib compression of the service component multiplex (ServEncID 107).
const ENC_ZLIB: u8 = 107;
/// Uncompressed service frames are at most 8 kB; anything far larger is not TPEG.
const INFLATE_LIMIT: u64 = 256 << 10;
const AID_SNI: u16 = 0;
const AID_TEC: u16 = 5;

#[derive(Default)]
pub struct TpegDecoder {
    buf: Vec<u8>,
    /// The last transport frame was good, the next one is due at the start of `buf`.
    synced: bool,
    services: BTreeMap<u32, ServiceState>,
    events: BTreeMap<EventKey, TrafficEvent>,
    changed: BTreeSet<EventKey>,
    /// Time of the last `expire`: messages expired by then are not taken.
    now: Option<u64>,
    stats: TpegStats,
}

impl TpegDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (another service or ensemble).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One MSC data group of the TPEG packet mode component.
    pub fn push_data_group(&mut self, dg: &[u8]) {
        self.stats.data_groups += 1;
        match crate::mot::parse_data_group(dg) {
            Some(g) => self.push(g.data),
            None => self.stats.data_group_errors += 1,
        }
    }

    /// Bytes of the TPEG stream, in any chunks.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.buf);
        let mut pos = 0;
        loop {
            let rest = &buf[pos..];
            let Some(i) = rest.windows(2).position(|w| w == [0xFF, 0x0F]) else {
                // keep a 0xFF that may start the sync word
                let keep = usize::from(rest.last() == Some(&0xFF));
                self.skip(rest.len() - keep);
                pos = buf.len() - keep;
                break;
            };
            if i > 0 {
                self.skip(i);
                pos += i;
                continue;
            }
            if rest.len() < 7 {
                break;
            }
            let len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
            let head_crc = u16::from_be_bytes([rest[4], rest[5]]);
            let checked = 7 + len.min(11);
            if rest.len() < checked {
                break;
            }
            let mut head = [0u8; 16];
            head[..4].copy_from_slice(&rest[..4]);
            head[4] = rest[6];
            head[5..5 + checked - 7].copy_from_slice(&rest[7..checked]);
            if crc16_ccitt(&head[..checked - 2]) != head_crc {
                if self.synced {
                    self.stats.header_crc_errors += 1;
                }
                self.skip(1);
                pos += 1;
                continue;
            }
            if rest.len() < 7 + len {
                break;
            }
            self.stats.frames += 1;
            if self.frame(rest[6], &rest[7..7 + len]) {
                self.synced = true;
                pos += 7 + len;
            } else {
                // look for frames inside this one: it may hide a gap in the stream
                self.synced = false;
                pos += 2;
            }
        }
        self.buf = buf;
        self.buf.drain(..pos);
    }

    fn skip(&mut self, n: usize) {
        if n > 0 {
            self.stats.skipped_bytes += n as u64;
            self.synced = false;
        }
    }

    /// Drop messages whose expiry time is before `now` (seconds since 1970
    /// UTC); later messages that expired by then are not taken either.
    /// Returns how many were dropped.
    pub fn expire(&mut self, now: u64) -> usize {
        self.now = Some(now);
        let gone: Vec<EventKey> = self.events.iter().filter(|(_, e)| e.expires < now).map(|(k, _)| *k).collect();
        for k in &gone {
            self.events.remove(k);
            self.changed.insert(*k);
        }
        self.stats.expired += gone.len() as u64;
        gone.len()
    }

    /// The current messages, by key.
    pub fn messages(&self) -> impl Iterator<Item = &TrafficEvent> {
        self.events.values()
    }

    pub fn get(&self, key: &EventKey) -> Option<&TrafficEvent> {
        self.events.get(key)
    }

    /// Keys of the messages added, changed or removed since the last call; a
    /// key `get` does not find any more was removed. A new expiry time,
    /// generation time or priority alone (message management) is not a change.
    pub fn take_changed(&mut self) -> Vec<EventKey> {
        std::mem::take(&mut self.changed).into_iter().collect()
    }

    /// The services described by SNI so far.
    pub fn services(&self) -> Vec<TpegService> {
        self.services
            .iter()
            .map(|(&sid, s)| TpegService {
                sid: sid_text(sid),
                name: s.name.clone(),
                description: s.description.clone(),
                components: s
                    .apps
                    .iter()
                    .map(|(&scid, &(aid, encrypted))| TpegComponent {
                        scid,
                        aid,
                        application: tables::application(aid),
                        version: s.versions.get(&scid).map(|(a, b)| format!("{a}.{b}")),
                        content: s.contents.get(&scid).cloned(),
                        encrypted,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn stats(&self) -> &TpegStats {
        &self.stats
    }

    fn app(&mut self, aid: u16) -> &mut AppStats {
        let apps = &mut self.stats.apps;
        let i = match apps.binary_search_by_key(&aid, |a| a.aid) {
            Ok(i) => i,
            Err(i) => {
                apps.insert(i, AppStats { aid, name: tables::application(aid), ..Default::default() });
                i
            }
        };
        &mut apps[i]
    }

    /// A transport frame with a good header; false when its content is broken.
    fn frame(&mut self, frame_type: u8, body: &[u8]) -> bool {
        match frame_type {
            0 => {
                // stream directory: number of services, their SIDs, data CRC
                if body.len() < 3 || body.len() != 3 + 3 * body[0] as usize || !check_crc16(body) {
                    self.stats.data_crc_errors += 1;
                    return false;
                }
                self.stats.stream_directories += 1;
                true
            }
            1 => self.service_frame(body),
            _ => true,
        }
    }

    fn service_frame(&mut self, body: &[u8]) -> bool {
        if body.len() < 4 {
            return false;
        }
        let sid = u32::from_be_bytes([0, body[0], body[1], body[2]]);
        let data: Cow<[u8]> = match body[3] {
            0 => Cow::Borrowed(&body[4..]),
            ENC_ZLIB => match inflate(&body[4..]) {
                Some(d) => Cow::Owned(d),
                None => {
                    self.stats.inflate_errors += 1;
                    return false;
                }
            },
            _ => {
                self.stats.encrypted += 1;
                return true;
            }
        };
        let mut r = Reader::new(&data);
        let mut ok = true;
        while !r.is_empty() {
            let (Some(scid), Some(len), Some(head_crc)) = (r.u8(), r.u16(), r.u16()) else {
                return false;
            };
            let Some(comp) = r.bytes(len as usize) else {
                return false;
            };
            let mut head = vec![scid];
            head.extend(len.to_be_bytes());
            head.extend(&comp[..comp.len().min(13)]);
            if crc16_ccitt(&head) != head_crc {
                self.stats.header_crc_errors += 1;
                return false;
            }
            ok &= self.component(sid, scid, comp);
        }
        ok
    }

    fn component(&mut self, sid: u32, scid: u8, data: &[u8]) -> bool {
        if scid == 0 {
            return self.sni(sid, data);
        }
        let Some(&(aid, encrypted)) = self.services.get(&sid).and_then(|s| s.apps.get(&scid)) else {
            self.stats.unknown_components += 1;
            return true;
        };
        self.app(aid).frames += 1;
        if encrypted {
            self.stats.encrypted += 1;
            return true;
        }
        if aid == AID_TEC {
            return self.tec(sid, scid, data);
        }
        if let Some(n) = count_messages(data) {
            self.app(aid).messages += n as u64;
        }
        true
    }

    /// SNI: message count, SNI components (id, 16 bit length, data), data CRC.
    fn sni(&mut self, sid: u32, data: &[u8]) -> bool {
        self.app(AID_SNI).frames += 1;
        if data.len() < 3 || !check_crc16(data) {
            self.stats.data_crc_errors += 1;
            return false;
        }
        let body = &data[..data.len() - 2];
        let svc = self.services.entry(sid).or_default();
        let mut r = Reader::new(&body[1..]);
        let mut n = 0u64;
        while !r.is_empty() {
            let (Some(id), Some(len)) = (r.u8(), r.u16()) else { break };
            let Some(a) = r.bytes(len as usize) else { break };
            n += 1;
            let mut a = Reader::new(a);
            match id {
                // service name and description
                0 => {
                    svc.name = a.short_string();
                    svc.description = a.short_string();
                }
                1 => fast_tuning(svc, &mut a),
                // content description
                3 => {
                    a.u8();
                    while let (Some(scid), Some(text)) = (a.u8(), a.short_string()) {
                        svc.contents.insert(scid, text);
                    }
                }
                // specification versions
                14 => {
                    a.u8();
                    while let (Some(scid), Some(major), Some(minor)) = (a.u8(), a.u8(), a.u8()) {
                        svc.versions.insert(scid, (major, minor));
                    }
                }
                _ => {}
            }
        }
        if n != body[0] as u64 {
            self.stats.message_errors += 1;
        }
        self.app(AID_SNI).messages += n;
        true
    }

    /// TEC: group priority, message count, messages, data CRC.
    fn tec(&mut self, sid: u32, scid: u8, data: &[u8]) -> bool {
        if data.len() < 4 || !check_crc16(data) {
            self.stats.data_crc_errors += 1;
            return false;
        }
        let body = &data[..data.len() - 2];
        let mut r = Reader::new(&body[2..]);
        let mut n = 0;
        while !r.is_empty() {
            let Some(msg) = r.component() else {
                self.stats.message_errors += 1;
                break;
            };
            n += 1;
            match tec::decode(&msg) {
                Some((m, ev)) => self.store(EventKey { sid, scid, id: m.id }, m.cancel, ev),
                None => self.stats.message_errors += 1,
            }
        }
        if n != body[1] as u64 {
            self.stats.message_errors += 1;
        }
        self.app(AID_TEC).messages += n;
        true
    }

    fn store(&mut self, key: EventKey, cancel: bool, mut ev: TrafficEvent) {
        if cancel {
            self.stats.cancellations += 1;
            if self.events.remove(&key).is_some() {
                self.changed.insert(key);
            }
            return;
        }
        if self.now.is_some_and(|now| ev.expires < now) {
            return;
        }
        ev.key = key.to_string();
        ev.sid = sid_text(key.sid);
        ev.scid = key.scid;
        if let Some(old) = self.events.get_mut(&key) {
            // the carousel moves the expiry time on: not a change of the message
            let same = TrafficEvent { expires: old.expires, generated: old.generated, priority: old.priority, ..ev.clone() } == *old;
            if same {
                (old.expires, old.generated, old.priority) = (ev.expires, ev.generated, ev.priority);
                return;
            }
        }
        self.events.insert(key, ev);
        self.changed.insert(key);
    }
}

/// SNI fast tuning table (guide to the service table 1): which application
/// every service component carries.
fn fast_tuning(svc: &mut ServiceState, a: &mut Reader) {
    // table version, character table
    if a.bytes(2).is_none() {
        return;
    }
    let mut apps = BTreeMap::new();
    let _ = (|| {
        while !a.is_empty() {
            let scid = a.u8()?;
            let flags = a.u8()?;
            if flags & 0x01 != 0 {
                // originator SID
                a.bytes(3)?;
            }
            a.u8()?; // content ID
            let aid = a.u16()?;
            if flags & 0x04 != 0 {
                // operating time
                a.bytes(8)?;
            }
            let mut encrypted = false;
            if flags & 0x08 != 0 {
                encrypted = a.u8()? != 0;
            }
            apps.insert(scid, (aid, encrypted));
        }
        Some(())
    })();
    svc.apps = apps;
}

/// Messages in a component frame of an application this decoder does not
/// decode: its data CRC must be right and the messages must add up to the
/// message count after a group priority, or without one.
fn count_messages(data: &[u8]) -> Option<u32> {
    if data.len() < 3 || !check_crc16(data) {
        return None;
    }
    let body = &data[..data.len() - 2];
    [2, 1].into_iter().find_map(|skip| {
        let count = *body.get(skip - 1)? as u32;
        let mut r = Reader::new(body.get(skip..)?);
        let mut n = 0;
        while !r.is_empty() {
            r.component()?;
            n += 1;
        }
        (n == count).then_some(n)
    })
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).take(INFLATE_LIMIT + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= INFLATE_LIMIT).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::binary::build::component;
    use super::tec::tests::{mmc_bytes, tec_message};
    use super::*;

    const SID: u32 = 0x0A_0B_0C;

    fn transport(frame_type: u8, body: &[u8]) -> Vec<u8> {
        let mut head = vec![0xFF, 0x0F];
        head.extend((body.len() as u16).to_be_bytes());
        head.push(frame_type);
        head.extend(&body[..body.len().min(11)]);
        let crc = crc16_ccitt(&head);
        let mut f = vec![0xFF, 0x0F];
        f.extend((body.len() as u16).to_be_bytes());
        f.extend(crc.to_be_bytes());
        f.push(frame_type);
        f.extend(body);
        f
    }

    fn component_frame(scid: u8, data: &[u8]) -> Vec<u8> {
        let mut head = vec![scid];
        head.extend((data.len() as u16).to_be_bytes());
        head.extend(&data[..data.len().min(13)]);
        let crc = crc16_ccitt(&head);
        let mut f = head[..3].to_vec();
        f.extend(crc.to_be_bytes());
        f.extend(data);
        f
    }

    fn with_crc(mut d: Vec<u8>) -> Vec<u8> {
        let crc = crc16_ccitt(&d);
        d.extend(crc.to_be_bytes());
        d
    }

    fn service_frame(enc: u8, comps: &[Vec<u8>]) -> Vec<u8> {
        let mut mux: Vec<u8> = comps.concat();
        if enc == ENC_ZLIB {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&mux).unwrap();
            mux = e.finish().unwrap();
        }
        let mut body = SID.to_be_bytes()[1..].to_vec();
        body.push(enc);
        body.extend(mux);
        transport(1, &body)
    }

    /// SNI: name, fast tuning (SCID 1 TEC, SCID 2 TFP), versions, content.
    fn sni() -> Vec<u8> {
        let comp = |id: u8, a: &[u8]| -> Vec<u8> {
            let mut c = vec![id];
            c.extend((a.len() as u16).to_be_bytes());
            c.extend(a);
            c
        };
        let name = [&[4u8][..], b"Test", &[11], b"Test events"].concat();
        let tuning = [1, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 5, 2, 0, 2, 0, 7];
        let versions = [1, 1, 3, 2, 2, 1, 0];
        let content = [&[1u8, 1, 3][..], b"TEC"].concat();
        let mut d = vec![4];
        for c in [comp(0, &name), comp(1, &tuning), comp(14, &versions), comp(3, &content)] {
            d.extend(c);
        }
        component_frame(0, &with_crc(d))
    }

    fn tec_frame(msgs: &[Vec<u8>]) -> Vec<u8> {
        let mut d = vec![0, msgs.len() as u8];
        d.extend(msgs.concat());
        component_frame(1, &with_crc(d))
    }

    fn cancel(id: u32, version: u8) -> Vec<u8> {
        component(0, &[], &[&mmc_bytes(id, version, 2_000_000, true)])
    }

    #[test]
    fn sni_and_tec_in_plain_and_zlib_frames() {
        for enc in [0, ENC_ZLIB] {
            let mut d = TpegDecoder::new();
            d.push(&service_frame(enc, &[sni(), tec_frame(&[tec_message(1, 0, 2_000_000), tec_message(2, 0, 2_000_000)])]));
            let s = d.stats();
            assert_eq!((s.frames, s.header_crc_errors, s.data_crc_errors, s.message_errors), (1, 0, 0, 0));
            assert_eq!(s.apps.iter().map(|a| (a.name, a.frames, a.messages)).collect::<Vec<_>>(), [("SNI", 1, 4), ("TEC", 1, 2)]);
            let svc = &d.services()[0];
            assert_eq!(
                (svc.sid.as_str(), svc.name.as_deref(), svc.description.as_deref()),
                ("10.11.12", Some("Test"), Some("Test events"))
            );
            let tec = &svc.components[1];
            assert_eq!((tec.scid, tec.aid, tec.application), (1, 5, "TEC"));
            assert_eq!((tec.version.as_deref(), tec.content.as_deref()), (Some("3.2"), Some("TEC")));
            assert_eq!(svc.components[2].application, "TFP");
            let keys = d.take_changed();
            assert_eq!(keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["10.11.12/1/1", "10.11.12/1/2"]);
            let ev = d.get(&keys[1]).unwrap();
            assert_eq!((ev.key.as_str(), ev.id, ev.application), ("10.11.12/1/2", 2, "TEC"));
            assert_eq!(ev.causes[0].name, "Roadworks");
        }
    }

    #[test]
    fn updates_cancellations_and_expiry() {
        let mut d = TpegDecoder::new();
        let frame = |msgs: &[Vec<u8>]| service_frame(0, &[sni(), tec_frame(msgs)]);
        d.push(&frame(&[tec_message(1, 0, 2_000_000), tec_message(2, 0, 3_000_000)]));
        assert_eq!(d.take_changed().len(), 2);
        // the carousel again, with a later expiry time: nothing changed
        d.push(&frame(&[tec_message(1, 0, 2_000_000), tec_message(2, 0, 3_000_000)]));
        d.push(&frame(&[tec_message(2, 0, 3_000_100)]));
        assert!(d.take_changed().is_empty());
        assert_eq!(d.messages().map(|e| e.expires).collect::<Vec<_>>(), [2_000_000, 3_000_100]);
        // a new version of message 1
        d.push(&frame(&[tec_message(1, 1, 2_000_000)]));
        let k = d.take_changed();
        assert_eq!(k.len(), 1);
        assert_eq!(d.get(&k[0]).unwrap().version, 1);
        // message 2 cancelled, twice
        d.push(&frame(&[cancel(2, 1), cancel(2, 1)]));
        let k = d.take_changed();
        assert_eq!((k.len(), k[0].id), (1, 2));
        assert!(d.get(&k[0]).is_none());
        assert_eq!(d.stats().cancellations, 2);
        // message 1 expires
        assert_eq!(d.expire(2_000_001), 1);
        assert_eq!(d.take_changed()[0].id, 1);
        assert_eq!(d.messages().count(), 0);
        // and is not taken again from the carousel; a valid one is
        d.push(&frame(&[tec_message(1, 1, 2_000_000), tec_message(3, 0, 2_500_000)]));
        assert_eq!(d.messages().map(|e| e.id).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn chunks_junk_and_broken_frames() {
        let good = service_frame(ENC_ZLIB, &[sni(), tec_frame(&[tec_message(7, 0, 2_000_000)])]);
        let mut bad = good.clone();
        bad[5] ^= 1; // header CRC
        let mut broken = good.clone();
        let n = broken.len();
        broken[n - 3] ^= 0x55; // compressed data
        let mut stream = vec![0x0F, 0xFF, 0xFF, 0x0F, 1, 2, 3];
        stream.extend(&good);
        stream.extend(&bad);
        stream.extend(&broken);
        stream.extend(&good);
        let mut d = TpegDecoder::new();
        for b in &stream {
            d.push(std::slice::from_ref(b));
        }
        let s = d.stats().clone();
        assert_eq!(s.frames, 3);
        assert_eq!(s.header_crc_errors, 1);
        assert_eq!(s.inflate_errors, 1);
        assert!(s.skipped_bytes >= 7 + bad.len() as u64);
        assert_eq!(d.messages().count(), 1);
        // the same in one piece
        let mut d2 = TpegDecoder::new();
        d2.push(&stream);
        assert_eq!(*d2.stats(), s);
        assert!(d2.buf.len() < 2);
    }

    #[test]
    fn frames_across_data_groups() {
        let stream = [
            service_frame(0, &[sni()]),
            transport(0, &with_crc(vec![1, 10, 11, 12])),
            service_frame(0, &[tec_frame(&[tec_message(5, 0, 2_000_000)])]),
        ]
        .concat();
        let mut d = TpegDecoder::new();
        // data groups of type 0: header without CRC, then one with a CRC
        let (a, b) = stream.split_at(stream.len() / 2);
        d.push_data_group(&[&[0x00, 0x00][..], a].concat());
        d.push_data_group(&with_crc([&[0x40, 0x10][..], b].concat()));
        let mut bad = with_crc(vec![0x40, 0x20, 1, 2, 3]);
        bad[3] ^= 1;
        d.push_data_group(&bad);
        let s = d.stats();
        assert_eq!((s.data_groups, s.data_group_errors, s.frames, s.stream_directories), (3, 1, 3, 1));
        assert_eq!(d.messages().count(), 1);
    }

    #[test]
    fn components_before_their_sni_and_crc_errors() {
        let mut d = TpegDecoder::new();
        let tec = tec_frame(&[tec_message(1, 0, 2_000_000)]);
        d.push(&service_frame(0, std::slice::from_ref(&tec)));
        assert_eq!((d.stats().unknown_components, d.messages().count()), (1, 0));
        // a wrong data CRC: the frame is dropped, the next one is fine
        let mut bad = tec.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        d.push(&service_frame(0, &[sni(), bad]));
        assert_eq!((d.stats().data_crc_errors, d.messages().count()), (1, 0));
        d.push(&service_frame(0, &[sni(), tec]));
        assert_eq!(d.messages().count(), 1);
        // a wrong message count
        let mut d = TpegDecoder::new();
        let mut data = vec![0, 3];
        data.extend(tec_message(1, 0, 2_000_000));
        d.push(&service_frame(0, &[sni(), component_frame(1, &with_crc(data))]));
        assert_eq!((d.stats().message_errors, d.messages().count()), (1, 1));
    }

    #[test]
    fn other_applications_are_counted() {
        // TFP on SCID 2: group priority, count, two messages, data CRC
        let msg = component(0, &[], &[&component(1, &[1, 2, 3], &[])]);
        let tfp = component_frame(2, &with_crc([vec![0, 2], msg.clone(), msg].concat()));
        let mut d = TpegDecoder::new();
        d.push(&service_frame(0, &[sni(), tfp.clone(), tfp]));
        let tfp = d.stats().apps.iter().find(|a| a.aid == 7).unwrap();
        assert_eq!((tfp.name, tfp.frames, tfp.messages), ("TFP", 2, 4));
        assert_eq!(d.messages().count(), 0);
        // an encrypted service frame
        let mut f = service_frame(0, &[sni()]);
        f[10] = 200;
        let mut g = f.clone();
        g.splice(4..6, transport(1, &f[7..])[4..6].iter().copied());
        d.push(&g);
        assert_eq!(d.stats().encrypted, 1);
    }

    #[test]
    fn json_of_a_message() {
        let mut d = TpegDecoder::new();
        d.push(&service_frame(0, &[sni(), tec_frame(&[tec_message(42, 3, 2_000_000)])]));
        let v = serde_json::to_value(d.messages().next().unwrap()).unwrap();
        assert_eq!(v["key"], "10.11.12/1/42");
        assert_eq!(v["sid"], "10.11.12");
        assert_eq!((v["id"].as_u64(), v["version"].as_u64(), v["expires"].as_u64()), (Some(42), Some(3), Some(2_000_000)));
        assert_eq!(v["effect"], serde_json::json!({"code": 5, "name": "Queuing traffic"}));
        assert_eq!(v["causes"][0]["subName"], "Major roadworks");
        assert_eq!(v["causes"][0]["laneRestriction"]["code"], 1);
        assert_eq!(v["warning"]["code"], 3);
        assert_eq!(v["location"]["method"], "OLR");
        assert_eq!(v["location"]["points"].as_array().unwrap().len(), 2);
        assert_eq!(v["location"]["tmc"]["location"], 12345);
        assert_eq!(v["lengthAffected"], 2500);
        assert!(v.get("averageSpeed").is_none());
        let s = serde_json::to_value(d.stats()).unwrap();
        assert_eq!(s["apps"][1]["name"], "TEC");
    }
}
