//! Changes of the service information of the stations (the SI document of
//! SPI, as broadcast): names, description, genres, links, logos, RadioDNS.
//! A logo counts by its sizes there, and each of its pictures (a new logo of
//! the same size) by a hash of it as it comes (spi.rs). The change log
//! (changes.rs) gets a new value once it came twice, half a minute apart,
//! with nothing else between; a value the field had before (documents of a
//! carousel that differ, a name by day and by night, two pictures of one
//! size taking turns) is no news. What is seen for the first time is
//! learnt, not reported.

use magicdab_dab::spi::ServiceInfo;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A new value counts once it came again after this long.
pub const SI_CONFIRM: Duration = Duration::from_secs(30);
/// Values of a field remembered (the current one last).
const HISTORY: usize = 4;

/// The fields of a station's SI as compared: (change kind, value).
pub type SiFields = Vec<(&'static str, String)>;

/// The fields of a service of an SI document.
pub fn fields_of(s: &ServiceInfo) -> SiFields {
    let text = |v: &Option<String>| v.as_deref().map(str::trim).unwrap_or("").to_string();
    // (a genre by its name when the document gives one, else by its code)
    let mut genres: Vec<String> = s.genres.iter().map(|g| g.rsplit(':').next().unwrap_or(g).trim().to_string()).collect();
    genres.sort();
    genres.dedup();
    let mut links = s.links.clone();
    links.sort();
    links.dedup();
    // logos by their sizes: some encoders name the files anew now and then
    let mut logos: Vec<(u16, u16)> = s.logos.iter().map(|l| (l.width, l.height)).collect();
    logos.sort();
    logos.dedup();
    let logos: Vec<String> = logos.iter().map(|(w, h)| if *w > 0 { format!("{w}×{h}") } else { "?".into() }).collect();
    vec![
        ("siShortName", text(&s.short_name)),
        ("siMediumName", text(&s.medium_name)),
        ("siLongName", text(&s.long_name)),
        ("siDescription", text(&s.description)),
        ("siGenres", genres.join(", ")),
        ("siLinks", links.join(" ")),
        ("siLogos", logos.join(", ")),
        ("siRadioDns", s.radiodns.as_ref().map(|(f, i)| format!("{f} / {i}")).unwrap_or_default()),
    ]
}

#[derive(Default, Serialize, Deserialize)]
struct Known {
    /// Per station ("E24A91"): per field, its values (the current one last).
    services: HashMap<String, BTreeMap<String, Vec<String>>>,
}

pub struct SiTracker {
    path: PathBuf,
    known: Known,
    /// New values waiting: (station, field) -> (value, first come, times).
    pending: HashMap<(String, String), (String, Instant, u32)>,
}

impl SiTracker {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("si-known.json");
        let known = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        SiTracker { path, known, pending: HashMap::new() }
    }

    fn save(&self) {
        if let Ok(s) = serde_json::to_string(&self.known) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, &self.path);
            }
        }
    }

    /// The SI of station `key` as just received: the changes confirmed now
    /// (kind, from, to).
    pub fn seen(&mut self, key: &str, fields: SiFields, now: Instant) -> Vec<(&'static str, String, String)> {
        let mut out = Vec::new();
        for (kind, value) in fields {
            if let Some((from, to)) = self.check(key, kind, value, now) {
                out.push((kind, from, to));
            }
        }
        out
    }

    /// A logo of station `key` as received (`size` "600×600", `hash` of its
    /// picture): true when another picture of that size is confirmed now.
    pub fn logo_seen(&mut self, key: &str, size: &str, hash: &str, now: Instant) -> bool {
        self.check(key, &format!("logo {size}"), hash.to_string(), now).is_some()
    }

    /// A value of a field: the change (from, to) once it is confirmed and new;
    /// the first value of a field is learnt.
    fn check(&mut self, key: &str, field: &str, value: String, now: Instant) -> Option<(String, String)> {
        let history = self.known.services.entry(key.to_string()).or_default().entry(field.to_string()).or_default();
        if history.is_empty() {
            history.push(value);
            self.save();
            return None;
        }
        let current = history.last().cloned().unwrap_or_default();
        let pk = (key.to_string(), field.to_string());
        if value == current {
            self.pending.remove(&pk);
            return None;
        }
        let p = self.pending.entry(pk.clone()).or_insert_with(|| (value.clone(), now, 0));
        if p.0 != value {
            *p = (value.clone(), now, 0);
        }
        p.2 += 1;
        if p.2 < 2 || now.duration_since(p.1) < SI_CONFIRM {
            return None;
        }
        self.pending.remove(&pk);
        let history = self.known.services.entry(key.to_string()).or_default().entry(field.to_string()).or_default();
        let had = history.contains(&value);
        history.retain(|v| *v != value);
        history.push(value.clone());
        if history.len() > HISTORY {
            history.remove(0);
        }
        self.save();
        (!had).then_some((current, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicdab_dab::spi::LogoRef;

    fn info(name: &str, logos: &[(u16, u16)]) -> ServiceInfo {
        ServiceInfo {
            medium_name: Some(name.into()),
            genres: vec!["urn:tva:metadata:cs:ContentCS:2004:3.6.4.14".into(), "Pop".into()],
            logos: logos
                .iter()
                .map(|&(width, height)| LogoRef { url: format!("/l{width}.png"), mime: None, width, height, kind: None })
                .collect(),
            ..Default::default()
        }
    }

    fn tracker(tag: &str) -> (SiTracker, PathBuf) {
        let d = std::env::temp_dir().join(format!("magicdab-si-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (SiTracker::load(&d), d)
    }

    #[test]
    fn a_new_value_counts_once_it_came_twice() {
        let (mut t, d) = tracker("twice");
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        // first seen: learnt
        assert!(t.seen("E24A91", fields_of(&info("Radio A", &[(32, 32)])), at(0)).is_empty());
        // a new name once, then the old one again: nothing
        assert!(t.seen("E24A91", fields_of(&info("Radio B", &[(32, 32)])), at(60)).is_empty());
        assert!(t.seen("E24A91", fields_of(&info("Radio A", &[(32, 32)])), at(120)).is_empty());
        assert!(t.seen("E24A91", fields_of(&info("Radio B", &[(32, 32)])), at(180)).is_empty());
        // twice in a row, half a minute apart: reported, with the logos
        let c = t.seen("E24A91", fields_of(&info("Radio B", &[(32, 32), (600, 600)])), at(240));
        assert_eq!(c, vec![("siMediumName", "Radio A".to_string(), "Radio B".to_string())]);
        let c = t.seen("E24A91", fields_of(&info("Radio B", &[(32, 32), (600, 600)])), at(300));
        assert_eq!(c, vec![("siLogos", "32×32".to_string(), "32×32, 600×600".to_string())]);
        // back to a name it had: no news (but it is the name now)
        assert!(t.seen("E24A91", fields_of(&info("Radio A", &[(32, 32), (600, 600)])), at(360)).is_empty());
        assert!(t.seen("E24A91", fields_of(&info("Radio A", &[(32, 32), (600, 600)])), at(420)).is_empty());
        assert!(t.seen("E24A91", fields_of(&info("Radio A", &[(32, 32), (600, 600)])), at(480)).is_empty());
        // remembered over a restart
        let mut again = SiTracker::load(&d);
        let c = again.seen("E24A91", fields_of(&info("Radio C", &[(32, 32), (600, 600)])), at(500));
        assert!(c.is_empty());
        let c = again.seen("E24A91", fields_of(&info("Radio C", &[(32, 32), (600, 600)])), at(540));
        assert_eq!(c, vec![("siMediumName", "Radio A".to_string(), "Radio C".to_string())]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn another_picture_of_a_logo_is_news_two_taking_turns_are_not() {
        let (mut t, d) = tracker("logo");
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(!t.logo_seen("E24A91", "600×600", "aaaa", at(0)));
        assert!(!t.logo_seen("E24A91", "600×600", "aaaa", at(60)));
        // a colour and a mono picture of one size, taking turns: never twice in a row
        for i in 0..4 {
            assert!(!t.logo_seen("E24A91", "600×600", "bbbb", at(120 + i * 120)));
            assert!(!t.logo_seen("E24A91", "600×600", "aaaa", at(180 + i * 120)));
        }
        // a new one that stays
        assert!(!t.logo_seen("E24A91", "600×600", "cccc", at(1000)));
        assert!(t.logo_seen("E24A91", "600×600", "cccc", at(1060)));
        assert!(!t.logo_seen("E24A91", "600×600", "cccc", at(1120)));
        // another size is a field of its own
        assert!(!t.logo_seen("E24A91", "32×32", "dddd", at(1200)));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn genres_by_their_code_or_name_links_and_logos_in_order() {
        let mut s = info("Radio A", &[(600, 600), (32, 32), (32, 32)]);
        s.links = vec!["https://b.example".into(), "https://a.example".into()];
        s.radiodns = Some(("radio.example".into(), "a".into()));
        let f: HashMap<&str, String> = fields_of(&s).into_iter().collect();
        assert_eq!(f["siGenres"], "3.6.4.14, Pop");
        assert_eq!(f["siLogos"], "32×32, 600×600");
        assert_eq!(f["siLinks"], "https://a.example https://b.example");
        assert_eq!(f["siRadioDns"], "radio.example / a");
        assert_eq!(f["siDescription"], "");
    }
}
