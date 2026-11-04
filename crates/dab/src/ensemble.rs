//! Ensemble database built from the FIC.

use crate::puncture::Protection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Label {
    pub text: String,
    /// Short label as selected by the character flag field.
    pub short: String,
}

impl Label {
    pub fn new(text: String, flags: u16) -> Self {
        let mut short = String::new();
        for (i, c) in text.chars().enumerate().take(16) {
            if flags & (0x8000 >> i) != 0 {
                short.push(c);
            }
        }
        let text = text.trim_end().to_string();
        let short = short.trim_end().to_string();
        Label { text, short }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Subchannel {
    pub id: u8,
    pub start: u16,
    pub size: u16,
    pub protection: Protection,
    pub bitrate: u16,
    /// Human readable protection, e.g. "EEP 3-A" or "UEP 3".
    pub protection_label: String,
    pub code_rate: String,
    /// FEC scheme for packet mode (FIG 0/14).
    pub fec_scheme: u8,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserApp {
    pub ua_type: u16,
    /// X-PAD application type if carried in X-PAD.
    pub xpad_app_type: Option<u8>,
    pub dscty: Option<u8>,
    pub dg_flag: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    /// Transport mechanism: 0 audio stream, 1 data stream, 2 FIDC, 3 packet data.
    pub tmid: u8,
    /// ASCTy (audio) or DSCTy (data).
    pub ctype: u8,
    pub subch_id: Option<u8>,
    pub scid: Option<u16>,
    pub scids: Option<u8>,
    pub primary: bool,
    pub ca: bool,
    pub label: Option<Label>,
    pub language: Option<u8>,
    pub user_apps: Vec<UserApp>,
    /// Packet address (FIG 0/3).
    pub packet_address: Option<u16>,
    pub dg_flag: bool,
}

impl Component {
    pub fn is_audio(&self) -> bool {
        self.tmid == 0
    }

    pub fn is_dabplus(&self) -> bool {
        self.tmid == 0 && self.ctype == 63
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub sid: u32,
    /// 32 bit SId (data service).
    pub is_data: bool,
    pub label: Option<Label>,
    pub ecc: Option<u8>,
    pub pty: Option<u8>,
    pub pty_dynamic: bool,
    pub language: Option<u8>,
    pub ca_id: u8,
    pub components: Vec<Component>,
    /// Announcement support flags (FIG 0/18).
    pub asu: u16,
    pub clusters: Vec<u8>,
}

impl Service {
    pub fn country_id(&self) -> u8 {
        if self.is_data {
            ((self.sid >> 20) & 0xF) as u8
        } else {
            ((self.sid >> 12) & 0xF) as u8
        }
    }

    pub fn primary(&self) -> Option<&Component> {
        self.components.iter().find(|c| c.primary).or_else(|| self.components.first())
    }

    pub fn is_audio(&self) -> bool {
        self.primary().map(|c| c.is_audio()).unwrap_or(false)
    }
}

/// FIG 0/6 linkage set.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkageSet {
    pub lsn: u16,
    pub active: bool,
    pub hard: bool,
    pub international: bool,
    pub oe: bool,
    /// Key service of this ensemble (first id when OE = 0).
    pub key_sid: Option<u32>,
    pub ids: Vec<LinkedId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkedId {
    /// 0 = DAB SId, 1 = RDS PI, 2 = AM/FM legacy, 3 = DRM/AMSS.
    pub kind: u8,
    pub ecc: Option<u8>,
    pub id: u32,
}

/// FIG 0/21 frequency information entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrequencyInfo {
    pub region_id: u16,
    pub oe: bool,
    pub id: u32,
    /// Range & modulation: 0 DAB, 6 DRM, 8 FM with RDS, 9 FM without RDS, 10/12 AM, 14 AMSS.
    pub rm: u8,
    pub continuity: bool,
    pub frequencies: Vec<FrequencyEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrequencyEntry {
    pub khz: u32,
    /// DAB: geographically adjacent area.
    pub adjacent: Option<bool>,
    /// DAB: transmission mode (0 = not signalled).
    pub mode: Option<u8>,
}

/// FIG 0/24 other ensemble services.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OeService {
    pub sid: u32,
    pub ca_id: u8,
    pub eids: Vec<u16>,
}

/// FIG 0/19 announcement switching.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementSwitch {
    pub cluster: u8,
    pub asw: u16,
    pub new_flag: bool,
    pub subch_id: u8,
    pub region: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DabTime {
    pub mjd: u32,
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub millis: u16,
    pub lsi: bool,
}

impl DabTime {
    /// Seconds since the Unix epoch (UTC).
    pub fn unix(&self) -> i64 {
        (self.mjd as i64 - 40587) * 86400 + self.hours as i64 * 3600 + self.minutes as i64 * 60 + self.seconds as i64
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ensemble {
    pub eid: Option<u16>,
    pub ecc: Option<u8>,
    pub label: Option<Label>,
    /// Local time offset in half hours.
    pub lto: Option<i8>,
    pub inter_table_id: Option<u8>,
    pub alarm: bool,
    pub change_flags: u8,
    pub subchannels: BTreeMap<u8, Subchannel>,
    pub services: BTreeMap<u32, Service>,
    pub linkage: Vec<LinkageSet>,
    pub frequency_info: Vec<FrequencyInfo>,
    pub oe_services: Vec<OeService>,
    pub announcements: BTreeMap<u8, AnnouncementSwitch>,
    pub time: Option<DabTime>,
}

impl Ensemble {
    pub fn ueid(&self) -> Option<u32> {
        self.eid.map(|e| ((self.ecc.unwrap_or(0) as u32) << 16) | e as u32)
    }

    pub fn country_id(&self) -> Option<u8> {
        self.eid.map(|e| (e >> 12) as u8)
    }

    /// Component (and service) using the given sub-channel.
    pub fn component_for_subchannel(&self, subch: u8) -> Option<(&Service, &Component)> {
        self.services
            .values()
            .flat_map(|s| s.components.iter().map(move |c| (s, c)))
            .find(|(_, c)| c.subch_id == Some(subch) && c.tmid != 3)
    }

    pub fn component_for_scid(&self, scid: u16) -> Option<(&Service, &Component)> {
        self.services.values().flat_map(|s| s.components.iter().map(move |c| (s, c))).find(|(_, c)| c.scid == Some(scid))
    }

    pub fn used_cus(&self) -> u32 {
        self.subchannels.values().map(|s| s.size as u32).sum()
    }
}
