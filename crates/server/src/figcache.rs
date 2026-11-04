//! Cache of rarely transmitted FIC information per ensemble: FIG 0/6 service
//! linking, FIG 0/21 frequency information, FIG 0/24 other ensembles, FIG 0/17
//! programme type, FIG 0/5 language, FIG 0/18 announcement support and FIG 2
//! long labels. Some multiplexes repeat these only every few minutes; a
//! reopened multiplex shows the cached values (marked as such) until the live
//! ones arrive.

use crate::view::LongLabels;
use magicdab_dab::ensemble::{Ensemble, FrequencyInfo, LinkageSet, OeService};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Reception time after which the live data replaces the cache (all slow
/// FIGs have been repeated by then).
pub const COMPLETE_AFTER: Duration = Duration::from_secs(180);

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceSlow {
    pub pty: Option<u8>,
    pub language: Option<u8>,
    pub asu: u16,
    pub clusters: Vec<u8>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix time the data was last confirmed by live reception.
    pub updated: u64,
    pub linkage: Vec<LinkageSet>,
    pub frequency_info: Vec<FrequencyInfo>,
    pub oe_services: Vec<OeService>,
    pub services: BTreeMap<u32, ServiceSlow>,
    /// FIG 2 labels: (kind, id, scids) and text.
    pub labels: Vec<(u8, u32, u8, String)>,
}

impl Entry {
    fn from_live(ens: &Ensemble, labels: &LongLabels) -> Entry {
        let services = ens
            .services
            .values()
            .map(|s| (s.sid, ServiceSlow { pty: s.pty, language: s.language, asu: s.asu, clusters: s.clusters.clone() }))
            .filter(|(_, v)| v.pty.is_some() || v.language.is_some() || v.asu != 0)
            .collect();
        let mut labels: Vec<(u8, u32, u8, String)> = labels.iter().map(|(&(k, id, sc), t)| (k, id, sc, t.clone())).collect();
        labels.sort();
        Entry {
            updated: 0,
            linkage: ens.linkage.clone(),
            frequency_info: ens.frequency_info.clone(),
            oe_services: ens.oe_services.clone(),
            services,
            labels,
        }
    }

    fn is_empty(&self) -> bool {
        self.linkage.is_empty()
            && self.frequency_info.is_empty()
            && self.oe_services.is_empty()
            && self.services.is_empty()
            && self.labels.is_empty()
    }
}

fn same_set(a: &LinkageSet, b: &LinkageSet) -> bool {
    a.lsn == b.lsn && a.oe == b.oe && a.international == b.international && a.key_sid == b.key_sid
}

fn same_fi(a: &FrequencyInfo, b: &FrequencyInfo) -> bool {
    a.id == b.id && a.rm == b.rm && a.oe == b.oe
}

/// Add the cached items missing in `ens`.
fn merge_into(ens: &mut Ensemble, labels: &mut LongLabels, c: &Entry) {
    for set in &c.linkage {
        match ens.linkage.iter_mut().find(|l| same_set(l, set)) {
            Some(l) => {
                for id in &set.ids {
                    if !l.ids.contains(id) {
                        l.ids.push(id.clone());
                    }
                }
            }
            None => ens.linkage.push(set.clone()),
        }
    }
    for fi in &c.frequency_info {
        match ens.frequency_info.iter_mut().find(|f| same_fi(f, fi)) {
            Some(f) => {
                for e in &fi.frequencies {
                    if !f.frequencies.iter().any(|x| x.khz == e.khz) {
                        f.frequencies.push(e.clone());
                    }
                }
            }
            None => ens.frequency_info.push(fi.clone()),
        }
    }
    for oe in &c.oe_services {
        match ens.oe_services.iter_mut().find(|o| o.sid == oe.sid) {
            Some(o) => {
                for e in &oe.eids {
                    if !o.eids.contains(e) {
                        o.eids.push(*e);
                    }
                }
            }
            None => ens.oe_services.push(oe.clone()),
        }
    }
    for (sid, slow) in &c.services {
        if let Some(s) = ens.services.get_mut(sid) {
            if s.pty.is_none() {
                s.pty = slow.pty;
            }
            if s.language.is_none() {
                s.language = slow.language;
            }
            if s.asu == 0 && slow.asu != 0 {
                s.asu = slow.asu;
                s.clusters = slow.clusters.clone();
            }
        }
    }
    for (k, id, sc, t) in &c.labels {
        labels.entry((*k, *id, *sc)).or_insert_with(|| t.clone());
    }
}

pub struct FigCache {
    path: PathBuf,
    entries: HashMap<String, Entry>,
    dirty: bool,
    last_save: Instant,
}

impl FigCache {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("figcache.json");
        let entries = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
        FigCache { path, entries, dirty: false, last_save: Instant::now() }
    }

    fn key(channel: &str, eid: u16) -> String {
        format!("{channel}:{eid:04X}")
    }

    /// Record live data; `complete` once the ensemble has been received for
    /// `COMPLETE_AFTER` (the live data then replaces the cache, so items that
    /// are no longer signalled disappear).
    pub fn observe(&mut self, channel: &str, ens: &Ensemble, labels: &LongLabels, complete: bool) {
        let Some(eid) = ens.eid else { return };
        let live = Entry::from_live(ens, labels);
        if live.is_empty() && !complete {
            return;
        }
        let entry = self.entries.entry(Self::key(channel, eid)).or_default();
        let mut next = if complete {
            live
        } else {
            let mut e = Ensemble::default();
            let mut l = LongLabels::new();
            merge_into(&mut e, &mut l, &live);
            merge_into(&mut e, &mut l, entry);
            let mut merged = Entry::from_live(&e, &l);
            merged.services = entry.services.clone();
            merged.services.extend(live.services);
            merged
        };
        next.updated = entry.updated;
        if next != *entry || complete {
            next.updated = crate::servicelist::now().max(0) as u64;
            *entry = next;
            self.dirty = true;
        }
    }

    /// Ensemble and labels with the cached items added, and the cache time,
    /// when the cache adds anything to the live data.
    pub fn merged(&self, channel: &str, ens: &Ensemble, labels: &LongLabels) -> Option<(Ensemble, LongLabels, u64)> {
        let c = self.entries.get(&Self::key(channel, ens.eid?))?;
        let mut e = ens.clone();
        let mut l = labels.clone();
        merge_into(&mut e, &mut l, c);
        if e == *ens && l == *labels {
            return None;
        }
        Some((e, l, c.updated))
    }

    pub fn save_if_due(&mut self) {
        if !self.dirty || self.last_save.elapsed() < Duration::from_secs(60) {
            return;
        }
        self.last_save = Instant::now();
        self.dirty = false;
        if let Ok(d) = serde_json::to_vec(&self.entries) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, d).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

/// Mark the parts of the merged ensemble JSON that are not in the live JSON.
pub fn flag_cached(merged: &mut Value, live: &Value, time: u64) {
    merged["cacheTime"] = Value::from(time);
    for key in ["frequencyInfo", "linkage", "oeServices"] {
        flag_items(&mut merged[key], &live[key]);
    }
    let live_services: HashMap<u64, &Value> =
        live["services"].as_array().into_iter().flatten().filter_map(|s| Some((s["sid"].as_u64()?, s))).collect();
    if let Some(services) = merged["services"].as_array_mut() {
        for s in services {
            let Some(ls) = s["sid"].as_u64().and_then(|sid| live_services.get(&sid)) else { continue };
            for key in ["links", "frequencies", "ensembles"] {
                let live_items = ls["linking"][key].clone();
                flag_items(&mut s["linking"][key], &live_items);
            }
            let fields: Vec<Value> =
                ["pty", "language", "asu", "long"].iter().filter(|&&f| s[f] != ls[f]).map(|&f| Value::from(f)).collect();
            if !fields.is_empty() {
                s["cachedFields"] = Value::Array(fields);
            }
        }
    }
    if merged["long"] != live["long"] {
        merged["longCached"] = Value::Bool(true);
    }
}

fn flag_items(merged: &mut Value, live: &Value) {
    let live_items = live.as_array().cloned().unwrap_or_default();
    if let Some(items) = merged.as_array_mut() {
        for it in items {
            if !live_items.contains(it) {
                it["cached"] = Value::Bool(true);
            }
        }
    }
}
