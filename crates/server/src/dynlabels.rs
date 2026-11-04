//! Services that change their label all the time, like a scrolling RDS PS on
//! FM (" BETA FM" -> " TAYLOR" -> "SWIFT -" ... every few seconds). Listeners
//! see the label live, but the service list and the change log keep the usual
//! name, so that notifications do not report every step. What is learned is
//! kept per multiplex (data/dynlabels.json): coming back, the service is known
//! from the start.

use magicdab_dab::ensemble::{Ensemble, Label};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

/// This many label changes within `WINDOW` make a label dynamic.
const CHANGES: usize = 3;
const WINDOW: Duration = Duration::from_secs(180);
/// Labels remembered per service (the least seen are dropped).
const MAX_LABELS: usize = 32;
/// A dynamic label that has not changed for this long is the name again: the
/// station stopped scrolling it (its frames would otherwise keep the old name,
/// since a label shown without a break never recurs).
const SETTLE: Duration = Duration::from_secs(30 * 60);

/// A label of the service: how often and how long (seconds) it was shown.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Frame {
    text: String,
    count: u32,
    secs: f64,
}

#[derive(Default)]
struct Seen {
    /// Labels in the order first seen; receivers judge them by display time.
    frames: Vec<Frame>,
    /// The label shown now, since when.
    current: Option<(String, Instant)>,
    changes: VecDeque<Instant>,
    dynamic: bool,
    /// The usual name, fixed once the label is known to be dynamic.
    pinned: Option<String>,
    /// The station's name from elsewhere (SPI / RadioDNS).
    hint: Option<String>,
}

/// Frames that are no station name ("DAB+" between the name and the songs).
fn generic(label: &str) -> bool {
    let l = label.trim().to_uppercase();
    l.is_empty()
        || ["DAB", "DAB+", "DAB +", "DIGITAL", "FM", "AM", "RADIO", "ONLINE", "HD", "LIVE", "ON AIR", "NEWS", "INFO"].contains(&l.as_str())
        || l.chars().all(|c| !c.is_alphabetic())
}

impl Seen {
    /// `label` is shown from `now` on: the one before was shown until now.
    fn show(&mut self, label: &str, now: Instant) {
        if let Some((cur, since)) = self.current.take() {
            if let Some(f) = self.frames.iter_mut().find(|f| f.text == cur) {
                f.secs += now.duration_since(since).as_secs_f64();
            }
        }
        match self.frames.iter_mut().find(|f| f.text == label) {
            Some(f) => f.count += 1,
            None => {
                if self.frames.len() >= MAX_LABELS {
                    let main = self.main().map(|s| s.to_string());
                    if let Some(i) = (0..self.frames.len())
                        .filter(|&i| Some(&self.frames[i].text) != main.as_ref())
                        .min_by(|&a, &b| self.frames[a].secs.total_cmp(&self.frames[b].secs))
                    {
                        self.frames.remove(i);
                    }
                }
                self.frames.push(Frame { text: label.to_string(), count: 1, secs: 0.0 });
            }
        }
        self.current = Some((label.to_string(), now));
    }

    fn secs(&self, label: &str) -> f64 {
        self.frames.iter().find(|f| f.text == label).map_or(0.0, |f| f.secs)
    }

    /// The name among the labels shown longest (within 70 % of the longest
    /// one), as receivers judge a scrolling label: the station's name from SPI
    /// when it is one of them, never a frame like "DAB+", one saying FM or
    /// RADIO before others, the longest and then the first seen.
    fn main(&self) -> Option<&str> {
        let top = self.frames.iter().filter(|f| !generic(&f.text)).map(|f| f.secs).fold(f64::NAN, f64::max);
        if top.is_nan() {
            return None;
        }
        let hint = self.hint.as_deref().map(|h| h.trim().to_lowercase());
        let score = |f: &Frame| {
            let u = f.text.trim().to_uppercase();
            let named = hint.as_deref() == Some(f.text.trim().to_lowercase().as_str());
            let station = u.split_whitespace().any(|w| w == "FM" || w == "RADIO" || w.starts_with("RADIO"));
            (named, station)
        };
        let mut best: Option<&Frame> = None;
        for f in self.frames.iter().filter(|f| !generic(&f.text) && f.secs >= top * 0.7) {
            if best.is_none_or(|b| (score(f), f.secs) > (score(b), b.secs)) {
                best = Some(f);
            }
        }
        best.map(|b| b.text.as_str())
    }
}

/// A service with a dynamic label as kept on disk ("8C_1A01_00001234").
#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    usual: Option<String>,
    /// The labels seen, how often and how long.
    frames: Vec<Frame>,
}

/// Seconds between writes of what was learned.
const SAVE_EVERY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct DynamicLabels {
    seen: HashMap<u32, Seen>,
    /// The multiplex ("8C_1A01"), once its EId is known.
    mux: Option<String>,
    known: HashMap<String, Saved>,
    dirty: bool,
    saved_at: Option<Instant>,
}

impl DynamicLabels {
    pub fn load(path: &Path) -> Self {
        let known = std::fs::read(path).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
        DynamicLabels { known, ..Default::default() }
    }

    /// Keep what was learned on this multiplex.
    fn remember(&mut self) {
        let Some(mux) = self.mux.clone() else { return };
        // the name in use (a pinned one may have given way to a longer shown one)
        let saved: Vec<(u32, Saved)> = self
            .seen
            .iter()
            .filter(|(_, s)| s.dynamic)
            .map(|(&sid, s)| (sid, Saved { usual: self.usual(sid).map(|u| u.to_string()), frames: s.frames.clone() }))
            .collect();
        for (sid, s) in saved {
            self.known.insert(format!("{mux}_{sid:08X}"), s);
        }
    }

    /// Write what was learned (at most once a minute unless `now`).
    pub fn save(&mut self, path: &Path, now: bool) {
        if !self.dirty || (!now && self.saved_at.is_some_and(|t| t.elapsed() < SAVE_EVERY)) {
            return;
        }
        self.remember();
        self.dirty = false;
        self.saved_at = Some(Instant::now());
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.known).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// The multiplex is identified: its services known to have dynamic labels
    /// are so from the start.
    pub fn set_mux(&mut self, mux: String) {
        let prefix = format!("{mux}_");
        for (key, saved) in &self.known {
            let Some(sid) = key.strip_prefix(&prefix).and_then(|s| u32::from_str_radix(s, 16).ok()) else { continue };
            let seen = self.seen.entry(sid).or_default();
            seen.dynamic = true;
            seen.pinned = seen.pinned.clone().or_else(|| saved.usual.clone());
            for f in &saved.frames {
                match seen.frames.iter_mut().find(|x| x.text == f.text) {
                    Some(x) => {
                        x.count += f.count;
                        x.secs += f.secs;
                    }
                    None => seen.frames.push(f.clone()),
                }
            }
        }
        self.mux = Some(mux);
    }

    /// Another multiplex: labels belong to it.
    pub fn clear(&mut self) {
        self.remember();
        self.seen.clear();
        self.mux = None;
    }

    /// Note the service labels of an ensemble update; returns the services
    /// whose label turned out to be dynamic, with their usual name.
    pub fn observe(&mut self, prev: &Ensemble, ens: &Ensemble, now: Instant) -> Vec<(u32, String)> {
        let mut became = Vec::new();
        for (sid, s) in &ens.services {
            let Some(label) = s.label.as_ref().map(|l| l.text.as_str()) else { continue };
            let before = prev.services.get(sid).and_then(|p| p.label.as_ref()).map(|l| l.text.as_str());
            let seen = self.seen.entry(*sid).or_default();
            if seen.current.is_none() {
                seen.show(label, now);
                continue;
            }
            if before.is_none_or(|b| b == label) {
                continue;
            }
            seen.show(label, now);
            if seen.dynamic {
                self.dirty = true;
            }
            seen.changes.push_back(now);
            while seen.changes.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
                seen.changes.pop_front();
            }
            if !seen.dynamic && seen.changes.len() >= CHANGES {
                seen.dynamic = true;
                self.dirty = true;
                became.push((*sid, seen.main().unwrap_or(label).to_string()));
            }
        }
        became
    }

    /// Services whose dynamic label has stood still for `SETTLE`: they are
    /// shown as they are again, and forgotten on this multiplex.
    pub fn settle(&mut self, now: Instant) -> Vec<u32> {
        let quiet: Vec<u32> = self
            .seen
            .iter()
            .filter(|(_, s)| s.dynamic && s.current.as_ref().is_some_and(|(_, since)| now.duration_since(*since) >= SETTLE))
            .map(|(&sid, _)| sid)
            .collect();
        for sid in &quiet {
            self.seen.remove(sid);
            if let Some(mux) = &self.mux {
                self.known.remove(&format!("{mux}_{sid:08X}"));
            }
            self.dirty = true;
        }
        quiet
    }

    pub fn any(&self) -> bool {
        self.seen.values().any(|s| s.dynamic)
    }

    /// The usual name of a service whose label is dynamic: the pinned one,
    /// until another one is clearly shown longer (a song text pinned by chance
    /// gives way to the station name shown between the songs).
    pub fn usual(&self, sid: u32) -> Option<&str> {
        let s = self.seen.get(&sid).filter(|s| s.dynamic)?;
        let count = |l: &str| s.frames.iter().find(|f| f.text == l).map_or(0, |f| f.count);
        // a frame like "DAB+" is no name, even if it was pinned
        let pinned = s.pinned.as_deref().filter(|p| !generic(p));
        match (pinned, s.main()) {
            (Some(p), Some(m)) if m != p && count(m) >= 3 && s.secs(m) >= 2.0 * s.secs(p) => Some(m),
            (Some(p), _) => Some(p),
            (None, m) => m,
        }
    }

    /// Fix the usual name (e.g. as the service list knows the station).
    pub fn pin(&mut self, sid: u32, name: String) {
        if let Some(s) = self.seen.get_mut(&sid) {
            s.pinned = Some(name);
            self.dirty = true;
        }
    }

    /// The station's name from elsewhere (SPI / RadioDNS) for choosing among
    /// its frames.
    pub fn set_hint(&mut self, sid: u32, name: Option<String>) {
        if let Some(s) = self.seen.get_mut(&sid) {
            s.hint = name;
        }
    }

    /// Services with dynamic labels.
    pub fn sids(&self) -> Vec<u32> {
        self.seen.iter().filter(|(_, s)| s.dynamic).map(|(&sid, _)| sid).collect()
    }

    /// The ensemble with the usual names of services with dynamic labels.
    pub fn stable(&self, ens: &Ensemble) -> Ensemble {
        let mut e = ens.clone();
        for sid in self.sids() {
            if let (Some(s), Some(main)) = (e.services.get_mut(&sid), self.usual(sid)) {
                let short: String = main.trim().chars().take(8).collect();
                s.label = Some(Label { text: main.to_string(), short });
            }
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicdab_dab::ensemble::Service;

    fn ens(label: &str) -> Ensemble {
        let mut e = Ensemble::default();
        e.services
            .insert(0x1234, Service { sid: 0x1234, label: Some(Label { text: label.into(), short: String::new() }), ..Default::default() });
        e
    }

    #[test]
    fn a_label_that_stops_scrolling_is_the_name_again() {
        let mut d = DynamicLabels::default();
        d.set_mux("10C_3ABC".into());
        let t0 = Instant::now();
        let mut prev = Ensemble::default();
        for (i, l) in ["RADIO ALFA", "HELLO TO", "TO ALL", "HELLO TO ALL"].iter().enumerate() {
            let e = ens(l);
            d.observe(&prev, &e, t0 + Duration::from_secs(5 * i as u64));
            prev = e;
        }
        d.pin(0x1234, "HELLO TO ALL".into());
        d.save(&std::env::temp_dir().join(format!("dynlabels-test-{}.json", std::process::id())), true);
        assert_eq!(d.usual(0x1234), Some("HELLO TO ALL"));
        // then the station shows its name only
        let e = ens("RADIO ALFA");
        d.observe(&prev, &e, t0 + Duration::from_secs(60));
        assert!(d.settle(t0 + Duration::from_secs(60) + SETTLE / 2).is_empty());
        assert_eq!(d.settle(t0 + Duration::from_secs(61) + SETTLE), vec![0x1234]);
        assert_eq!(d.usual(0x1234), None);
        assert_eq!(d.stable(&e).services[&0x1234].label.as_ref().unwrap().text, "RADIO ALFA");
        assert!(d.known.is_empty(), "forgotten on this multiplex");
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("dynlabels-test-{}.json", std::process::id())));
    }

    #[test]
    fn scrolling_label_keeps_the_station_name() {
        let mut d = DynamicLabels::default();
        let t0 = Instant::now();
        let steps = [" BETA FM", " TAYLOR", "SWIFT -", "PATIENT", "  ZERO", " BETA FM", " SELENA", "GOMEZ -", "BAD LIAR", " BETA FM"];
        let mut prev = Ensemble::default();
        let mut became = Vec::new();
        for (i, l) in steps.iter().enumerate() {
            let e = ens(l);
            became.extend(d.observe(&prev, &e, t0 + Duration::from_secs(4 * i as u64)));
            prev = e;
        }
        assert_eq!(became.len(), 1);
        assert!(d.any());
        let stable = d.stable(&ens("GOMEZ -"));
        assert_eq!(stable.services[&0x1234].label.as_ref().unwrap().text, " BETA FM");
        // the name recurring clearly wins over a pinned song text
        d.pin(0x1234, " TAYLOR".into());
        assert_eq!(d.usual(0x1234), Some(" BETA FM"));
    }

    #[test]
    fn pinned_name_stays_until_another_clearly_recurs() {
        let mut d = DynamicLabels::default();
        let t0 = Instant::now();
        let mut prev = Ensemble::default();
        let mut step = |d: &mut DynamicLabels, i: u64, l: &str| {
            let e = ens(l);
            d.observe(&prev, &e, t0 + Duration::from_secs(4 * i));
            prev = e;
        };
        for (i, l) in ["ABBA -", "SOS", " BETA FM", "QUEEN"].iter().enumerate() {
            step(&mut d, i as u64, l);
        }
        d.pin(0x1234, "SOS".into());
        assert_eq!(d.usual(0x1234), Some("SOS"));
        for (i, l) in ["SEAL -", " BETA FM", "KISS", " BETA FM", "ABBA -"].iter().enumerate() {
            step(&mut d, 4 + i as u64, l);
        }
        assert_eq!(d.usual(0x1234), Some(" BETA FM"));
    }

    #[test]
    fn a_frame_like_dab_is_no_name() {
        // a station: "DAB+", "RADIO ALFA", "HELLO TO ALL" equally often, songs between
        let mut d = DynamicLabels::default();
        let t0 = Instant::now();
        let mut prev = Ensemble::default();
        let mut i = 0;
        for _ in 0..6 {
            for l in ["DAB+", "RADIO ALFA", "HELLO TO ALL", "Ania"] {
                let e = ens(l);
                d.observe(&prev, &e, t0 + Duration::from_secs(4 * i));
                prev = e;
                i += 1;
            }
        }
        d.pin(0x1234, "DAB+".into());
        assert_eq!(d.usual(0x1234), Some("RADIO ALFA"));
        // the name from SPI decides
        d.set_hint(0x1234, Some("Hello to all".into()));
        assert_eq!(d.usual(0x1234), Some("HELLO TO ALL"));
    }

    #[test]
    fn the_frame_shown_longest_is_the_name() {
        // a slogan often but briefly, the name of the station longer
        let mut d = DynamicLabels::default();
        let t0 = Instant::now();
        let mut prev = Ensemble::default();
        let mut t = 0;
        for _ in 0..4 {
            for (l, secs) in [("Radio Alfa Miasto", 20), ("Best hits!", 3), ("Best", 3), ("Best hits!", 3)] {
                let e = ens(l);
                d.observe(&prev, &e, t0 + Duration::from_secs(t));
                prev = e;
                t += secs;
            }
        }
        assert_eq!(d.usual(0x1234), Some("Radio Alfa Miasto"));
    }

    #[test]
    fn known_on_the_next_visit() {
        let dir = std::env::temp_dir().join(format!("dynlabels-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dynlabels.json");
        let mut d = DynamicLabels::default();
        d.set_mux("8C_1A01".into());
        let t0 = Instant::now();
        let mut prev = Ensemble::default();
        for (i, l) in [" BETA FM", " TAYLOR", "SWIFT -", " BETA FM", "PATIENT"].iter().enumerate() {
            let e = ens(l);
            d.observe(&prev, &e, t0 + Duration::from_secs(4 * i as u64));
            prev = e;
        }
        d.save(&path, true);
        let mut again = DynamicLabels::load(&path);
        again.set_mux("8C_1A01".into());
        assert_eq!(again.usual(0x1234), Some(" BETA FM"));
        assert_eq!(again.stable(&ens("ZERO")).services[&0x1234].label.as_ref().unwrap().text, " BETA FM");
        // another multiplex knows nothing of it
        let mut other = DynamicLabels::load(&path);
        other.set_mux("5B_3EB7".into());
        assert!(!other.any());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_renamed_service_is_not_dynamic() {
        let mut d = DynamicLabels::default();
        let t0 = Instant::now();
        let (a, b) = (ens("Radio A"), ens("Radio B"));
        d.observe(&Ensemble::default(), &a, t0);
        d.observe(&a, &b, t0 + Duration::from_secs(60));
        assert!(!d.any());
        assert_eq!(d.stable(&b).services[&0x1234].label.as_ref().unwrap().text, "Radio B");
    }
}
