//! Persistent list of received ensembles and services (band scan results).

use magicdab_dab::audio::AudioFormat;
use magicdab_dab::ensemble::{Ensemble, Service};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEntry {
    pub sid: u32,
    pub scids: u8,
    pub label: String,
    pub short: String,
    pub pty: Option<u8>,
    pub codec: String,
    pub bitrate: u16,
    pub ecc: Option<u8>,
    /// E.g. "EEP 3-A"; empty when unknown.
    #[serde(default)]
    pub protection: String,
    /// FIG 0/5 language code.
    #[serde(default)]
    pub language: Option<u8>,
    /// Sub-channel of its audio.
    #[serde(default)]
    pub subch: Option<u8>,
    /// Conditional access (scrambled).
    #[serde(default)]
    pub ca: bool,
    /// User applications of its components (FIG 0/13: 2 slideshow, 7 SPI...).
    #[serde(default)]
    pub apps: Vec<u16>,
    /// Its audio as the decoder found it; unknown until it was played.
    #[serde(default)]
    pub format: Option<FormatEntry>,
}

/// A sub-channel (FIG 0/1): its place among the 864 capacity units.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct SubchEntry {
    pub id: u8,
    pub start: u16,
    pub size: u16,
    /// E.g. "EEP 3-A"; empty when unknown.
    pub protection: String,
    pub bitrate: u16,
}

/// The audio of a service as decoded.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct FormatEntry {
    /// "AAC-LC", "HE-AAC", "HE-AAC v2" or "MP2".
    pub codec: String,
    pub sample_rate: u32,
    pub stereo: bool,
}

impl FormatEntry {
    pub fn of(f: &AudioFormat) -> Self {
        FormatEntry { codec: f.codec.clone(), sample_rate: f.sample_rate, stereo: f.stereo }
    }
}

/// A data service (EPG, TPEG, Journaline...).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct DataEntry {
    pub sid: u32,
    pub label: String,
    pub short: String,
    pub subch: Option<u8>,
    /// Packet address; none for a data stream.
    pub packet: Option<u16>,
    pub apps: Vec<u16>,
    pub ca: bool,
}

/// The user applications of a service's components, each once.
fn apps(s: &Service) -> Vec<u16> {
    let mut a: Vec<u16> = s.components.iter().flat_map(|c| c.user_apps.iter().map(|u| u.ua_type)).collect();
    a.sort_unstable();
    a.dedup();
    a
}

fn scrambled(s: &Service) -> bool {
    s.ca_id != 0 || s.primary().is_some_and(|c| c.ca)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsembleEntry {
    pub channel: String,
    pub frequency: u32,
    pub eid: u16,
    pub ecc: u8,
    pub label: String,
    pub short: String,
    pub services: Vec<ServiceEntry>,
    /// Unix time of the last reception.
    pub last_seen: i64,
    #[serde(default)]
    pub snr: f32,
    #[serde(default)]
    pub tii: Vec<String>,
    /// The sub-channels, by start.
    #[serde(default)]
    pub subchannels: Vec<SubchEntry>,
    /// Data services (`services` are the radio stations).
    #[serde(default)]
    pub data: Vec<DataEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServiceList {
    pub ensembles: Vec<EnsembleEntry>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl ServiceList {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(path(dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        if let Ok(txt) = serde_json::to_string_pretty(self) {
            let p = path(dir);
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, txt).is_ok() {
                let _ = std::fs::rename(&tmp, p);
            }
        }
    }

    /// Insert or replace the entry of an ensemble received on `channel`.
    /// Returns true when the list changed.
    pub fn update(&mut self, channel: &str, frequency: u32, ens: &Ensemble, snr: f32) -> bool {
        let Some(eid) = ens.eid else { return false };
        let label = ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default();
        if label.is_empty() {
            return false;
        }
        let mut services: Vec<ServiceEntry> = ens
            .services
            .values()
            .filter(|s| s.is_audio() && s.label.is_some())
            .map(|s| {
                let p = s.primary();
                let sc = p.and_then(|c| c.subch_id).and_then(|id| ens.subchannels.get(&id));
                ServiceEntry {
                    sid: s.sid,
                    scids: 0,
                    label: s.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
                    short: s.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
                    pty: s.pty,
                    codec: if p.map(|c| c.is_dabplus()).unwrap_or(false) { "DAB+".into() } else { "DAB".into() },
                    bitrate: sc.map(|x| x.bitrate).unwrap_or(0),
                    ecc: s.ecc.or(ens.ecc),
                    protection: sc.map(|x| x.protection_label.clone()).unwrap_or_default(),
                    language: s.language.or_else(|| p.and_then(|c| c.language)),
                    subch: p.and_then(|c| c.subch_id),
                    ca: scrambled(s),
                    apps: apps(s),
                    format: None,
                }
            })
            .collect();
        services.sort_by_key(|a| a.label.to_lowercase());
        let mut subchannels: Vec<SubchEntry> = ens
            .subchannels
            .values()
            .map(|c| SubchEntry { id: c.id, start: c.start, size: c.size, protection: c.protection_label.clone(), bitrate: c.bitrate })
            .collect();
        subchannels.sort_by_key(|c| (c.start, c.id));
        let mut data: Vec<DataEntry> = ens
            .services
            .values()
            .filter(|s| !s.is_audio() && !s.components.is_empty())
            .map(|s| {
                let p = s.primary();
                DataEntry {
                    sid: s.sid,
                    label: s.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
                    short: s.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
                    subch: p.and_then(|c| c.subch_id),
                    packet: p.and_then(|c| c.packet_address),
                    apps: apps(s),
                    ca: scrambled(s),
                }
            })
            .collect();
        data.sort_by_key(|d| d.sid);
        let entry = EnsembleEntry {
            channel: channel.to_string(),
            frequency,
            eid,
            ecc: ens.ecc.unwrap_or(0),
            label,
            short: ens.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
            services,
            last_seen: now(),
            snr,
            tii: Vec::new(),
            subchannels,
            data,
        };
        // with a weak signal not all labels may have arrived yet: keep the
        // services known from earlier receptions until the list is complete
        let complete = ens.services.values().filter(|s| s.is_audio()).all(|s| s.label.is_some());
        let mut entry = entry;
        if let Some(old) = self.ensembles.iter().find(|e| e.channel == channel && e.eid == eid) {
            if !complete {
                for svc in &old.services {
                    if !entry.services.iter().any(|n| n.sid == svc.sid) {
                        entry.services.push(svc.clone());
                    }
                }
                entry.services.sort_by_key(|a| a.label.to_lowercase());
                for c in &old.subchannels {
                    if !entry.subchannels.iter().any(|n| n.id == c.id) {
                        entry.subchannels.push(c.clone());
                    }
                }
                entry.subchannels.sort_by_key(|c| (c.start, c.id));
                for d in &old.data {
                    if !entry.data.iter().any(|n| n.sid == d.sid) {
                        entry.data.push(d.clone());
                    }
                }
                entry.data.sort_by_key(|d| d.sid);
            }
            // the audio of a station stays known while its sub-channel stays
            for s in entry.services.iter_mut().filter(|s| s.format.is_none()) {
                if let Some(o) =
                    old.services.iter().find(|o| o.sid == s.sid && o.codec == s.codec && o.bitrate == s.bitrate && o.subch == s.subch)
                {
                    s.format = o.format.clone();
                }
            }
        }
        // this multiplex under a former EId (same name) gives way to it
        let name = entry.label.trim().to_lowercase();
        let count = self.ensembles.len();
        self.ensembles.retain(|e| !(e.channel == channel && e.eid != eid && e.label.trim().to_lowercase() == name));
        let replaced = self.ensembles.len() != count;
        match self.ensembles.iter_mut().find(|e| e.channel == channel && e.eid == eid) {
            Some(e) => {
                let changed = replaced
                    || e.services != entry.services
                    || e.label != entry.label
                    || e.short != entry.short
                    || e.subchannels != entry.subchannels
                    || e.data != entry.data;
                let tii = std::mem::take(&mut e.tii);
                *e = entry;
                e.tii = tii;
                changed
            }
            None => {
                // a different ensemble now occupies this channel: kept for a week
                // (DX: muxes alternate)
                self.ensembles.retain(|e| !(e.channel == channel && e.eid != eid && e.last_seen < now() - 7 * 86400));
                self.ensembles.push(entry);
                self.sort();
                true
            }
        }
    }

    pub fn sort(&mut self) {
        self.ensembles.sort_by(|a, b| a.frequency.cmp(&b.frequency).then(a.eid.cmp(&b.eid)));
    }

    /// The audio of a station as decoded, kept with it. True when the list changed.
    pub fn set_format(&mut self, channel: &str, eid: u16, sid: u32, f: &FormatEntry) -> bool {
        let found = self
            .ensembles
            .iter_mut()
            .filter(|e| e.channel == channel && e.eid == eid)
            .flat_map(|e| e.services.iter_mut())
            .find(|s| s.sid == sid);
        match found {
            Some(s) if s.format.as_ref() != Some(f) => {
                s.format = Some(f.clone());
                true
            }
            _ => false,
        }
    }

    pub fn remove_ensemble(&mut self, channel: &str, eid: u16) -> bool {
        let n = self.ensembles.len();
        self.ensembles.retain(|e| !(e.channel == channel && e.eid == eid));
        n != self.ensembles.len()
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join("servicelist.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicdab_dab::ensemble::{Component, Label, Subchannel, UserApp};
    use magicdab_dab::puncture::Protection;

    fn ens(eid: u16, label: &str) -> Ensemble {
        Ensemble { eid: Some(eid), label: Some(Label { text: label.into(), short: String::new() }), ..Default::default() }
    }

    fn subch(id: u8, start: u16, size: u16, bitrate: u16) -> Subchannel {
        Subchannel {
            id,
            start,
            size,
            protection: Protection::Eep { option: 0, level: 2 },
            bitrate,
            protection_label: "EEP 3-A".into(),
            code_rate: String::new(),
            fec_scheme: 0,
        }
    }

    fn component(tmid: u8, subch: u8, apps: &[u16], packet: Option<u16>) -> Component {
        let user_apps =
            apps.iter().map(|&ua_type| UserApp { ua_type, xpad_app_type: None, dscty: None, dg_flag: false, data: Vec::new() }).collect();
        Component {
            tmid,
            ctype: if tmid == 0 { 63 } else { 60 },
            subch_id: Some(subch),
            scid: None,
            scids: Some(0),
            primary: true,
            ca: false,
            label: None,
            language: None,
            user_apps,
            packet_address: packet,
            dg_flag: false,
        }
    }

    /// Two stations (one with a slideshow and SPI) and an EPG in packet mode.
    fn mux() -> Ensemble {
        let mut e = ens(0x4B6E, "MUX A");
        for (i, (sid, apps)) in [(0x4A91u32, &[7u16, 2][..]), (0x4A92, &[][..])].into_iter().enumerate() {
            let id = i as u8 + 1;
            e.subchannels.insert(id, subch(id, i as u16 * 72, 72, 96));
            let label = Some(Label { text: format!("Radio {i}"), short: String::new() });
            e.services.insert(sid, Service { sid, label, components: vec![component(0, id, apps, None)], ..Default::default() });
        }
        e.subchannels.insert(9, subch(9, 144, 12, 8));
        let label = Some(Label { text: "EPG".into(), short: String::new() });
        e.services.insert(
            0xE1C0_0001,
            Service { sid: 0xE1C0_0001, is_data: true, label, components: vec![component(3, 9, &[7], Some(1))], ..Default::default() },
        );
        e
    }

    #[test]
    fn the_whole_multiplex_is_kept() {
        let mut l = ServiceList::default();
        assert!(l.update("11B", 218640, &mux(), 20.0));
        let e = &l.ensembles[0];
        assert_eq!(
            e.subchannels.iter().map(|c| (c.id, c.start, c.size, c.bitrate)).collect::<Vec<_>>(),
            vec![(1, 0, 72, 96), (2, 72, 72, 96), (9, 144, 12, 8)]
        );
        assert_eq!(
            e.services.iter().map(|s| (s.sid, s.subch, s.apps.clone())).collect::<Vec<_>>(),
            vec![(0x4A91, Some(1), vec![2, 7]), (0x4A92, Some(2), vec![])]
        );
        assert_eq!(
            e.data,
            vec![DataEntry {
                sid: 0xE1C0_0001,
                label: "EPG".into(),
                short: String::new(),
                subch: Some(9),
                packet: Some(1),
                apps: vec![7],
                ca: false
            }]
        );
        // the same again (another SNR): no change
        assert!(!l.update("11B", 218640, &mux(), 18.0));
        // a reconfiguration
        let mut m = mux();
        m.subchannels.get_mut(&9).unwrap().size = 18;
        assert!(l.update("11B", 218640, &m, 18.0));
    }

    #[test]
    fn the_audio_format_stays_while_the_subchannel_does() {
        let mut l = ServiceList::default();
        l.update("11B", 218640, &mux(), 20.0);
        let f = FormatEntry { codec: "HE-AAC v2".into(), sample_rate: 48000, stereo: true };
        assert!(l.set_format("11B", 0x4B6E, 0x4A91, &f));
        assert!(!l.set_format("11B", 0x4B6E, 0x4A91, &f));
        assert!(!l.set_format("11B", 0x4B6E, 0x9999, &f));
        assert!(!l.update("11B", 218640, &mux(), 20.0));
        assert_eq!(l.ensembles[0].services[0].format.as_ref(), Some(&f));
        // another bitrate: the format is not known any more
        let mut m = mux();
        m.subchannels.get_mut(&1).unwrap().bitrate = 72;
        assert!(l.update("11B", 218640, &m, 20.0));
        assert_eq!(l.ensembles[0].services[0].format, None);
    }

    #[test]
    fn new_eid_of_the_same_multiplex_replaces_it() {
        let mut l = ServiceList::default();
        l.update("10C", 213360, &ens(0x3ABC, "MUX B"), 20.0);
        l.update("10C", 213360, &ens(0xC1A5, "MUX B  "), 20.0);
        assert_eq!(l.ensembles.iter().map(|e| e.eid).collect::<Vec<_>>(), vec![0xC1A5]);
        // another multiplex on the channel (DX) stays
        l.update("10C", 213360, &ens(0x6C2D, "MUX C"), 10.0);
        assert_eq!(l.ensembles.len(), 2);
        // both EIds already in the list (stale entry from before the fix)
        l.ensembles.push(EnsembleEntry { eid: 0x3ABC, ..l.ensembles.iter().find(|e| e.eid == 0xC1A5).unwrap().clone() });
        l.update("10C", 213360, &ens(0xC1A5, "MUX B"), 20.0);
        assert!(!l.ensembles.iter().any(|e| e.eid == 0x3ABC));
    }
}
