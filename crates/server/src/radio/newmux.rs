//! The first minute of a multiplex never heard here (changes.rs: ensembleNew):
//! its signal and its transmitters by TII are gathered while the receiver
//! stays on it (the band scan waits, as for a DX recording), then the report
//! goes with its event: into the Discord message posted at once, to the web
//! clients and into the change log.

use super::*;
use crate::changes::{ChangeEvent, DiscordMsg, Report, ReportTx};

/// How long a new multiplex is listened to for its report.
const WINDOW: Duration = Duration::from_secs(60);
/// The report goes early after this long without synchronisation (an
/// opening that closed).
const NO_SYNC: Duration = Duration::from_secs(15);

pub struct Probe {
    /// Of the event the report belongs to.
    time: i64,
    channel: String,
    ev_eid: String,
    eid: u16,
    ecc: Option<u8>,
    frequency: u32,
    started: Instant,
    last_sample: Option<Instant>,
    last_sync: Instant,
    /// Seconds with synchronisation, and their sums for the means.
    seconds: u32,
    snr_sum: f32,
    snr_max: f32,
    fib_sum: f32,
    /// TII code -> sum of its levels, seconds it was there.
    tii: HashMap<(u8, u8), (f32, u32)>,
    recording: Option<String>,
    discord: DiscordMsg,
}

impl Radio {
    /// From observe_changes: a new multiplex is listened to for its report.
    pub(super) fn newmux_start(&mut self, ev: &ChangeEvent, discord: DiscordMsg) {
        let Some(eid) = self.ensemble.eid else { return };
        // (another one before: its report goes with what it has)
        self.newmux_finish("retuned");
        self.newmux = Some(Probe {
            time: ev.time,
            channel: ev.channel.clone(),
            ev_eid: ev.eid.clone(),
            eid,
            ecc: self.ensemble.ecc,
            frequency: self.frequency,
            started: Instant::now(),
            last_sample: None,
            last_sync: Instant::now(),
            seconds: 0,
            snr_sum: 0.0,
            snr_max: f32::MIN,
            fib_sum: 0.0,
            tii: HashMap::new(),
            recording: self.dxrec_file(&ev.channel),
            discord,
        });
    }

    /// The band scan stays on a new multiplex for its report (radio/scan.rs).
    pub(super) fn newmux_holds_scan(&self) -> bool {
        self.newmux.as_ref().is_some_and(|p| self.channel.as_deref() == Some(p.channel.as_str()))
    }

    /// From the tick: a sample a second; the report at the end of the
    /// minute, when the signal went or the tuner left.
    pub(super) fn newmux_tick(&mut self) {
        let Some(p) = self.newmux.as_mut() else { return };
        let ended = if self.channel.as_deref() != Some(p.channel.as_str()) {
            Some("retuned")
        } else if p.started.elapsed() >= WINDOW {
            Some("")
        } else if p.last_sync.elapsed() > NO_SYNC {
            Some("lost")
        } else {
            None
        };
        if let Some(ended) = ended {
            self.newmux_finish(ended);
            return;
        }
        if p.last_sample.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        p.last_sample = Some(Instant::now());
        if !self.signal.synced {
            return;
        }
        p.last_sync = Instant::now();
        p.seconds += 1;
        p.snr_sum += self.signal.snr;
        p.snr_max = p.snr_max.max(self.signal.snr);
        p.fib_sum += self.signal.fib_error_rate;
        for c in &self.tii_codes {
            let e = p.tii.entry((c.main, c.sub)).or_default();
            e.0 += c.level_db;
            e.1 += 1;
        }
    }

    fn newmux_finish(&mut self, ended: &str) {
        let Some(p) = self.newmux.take() else { return };
        let n = p.seconds.max(1) as f32;
        let loc = self.settings.location.clone();
        let r1 = |v: f32| (v * 10.0).round() / 10.0;
        // (a code of a second or two is a false detection)
        let mut transmitters: Vec<ReportTx> = p
            .tii
            .iter()
            .filter(|(_, (_, k))| *k >= 3.min(p.seconds))
            .map(|(&(main, sub), &(sum, k))| {
                let tx = self.txdb.find(p.eid, p.ecc, main, sub, p.frequency);
                let (distance, azimuth) = match (&loc, tx) {
                    (Some(l), Some(t)) => {
                        let (d, a) = distance_azimuth(l.lat, l.lon, t.lat, t.lon);
                        (Some((d * 10.0).round() / 10.0), Some(a.round()))
                    }
                    _ => (None, None),
                };
                ReportTx {
                    code: format!("{main}-{sub}"),
                    level: r1(sum / k as f32),
                    location: tx.map(|t| t.location.clone()),
                    country: tx.map(|t| t.country.clone()).filter(|c| !c.is_empty()),
                    distance,
                    azimuth,
                    power_kw: tx.map(|t| t.power_kw),
                }
            })
            .collect();
        transmitters.sort_by(|a, b| b.level.total_cmp(&a.level));
        let report = Report {
            seconds: p.seconds,
            ended: ended.to_string(),
            snr_mean: r1(p.snr_sum / n),
            snr_max: if p.seconds > 0 { r1(p.snr_max) } else { 0.0 },
            fib_errors: (p.fib_sum / n * 1000.0).round() / 1000.0,
            country: p.ecc.and_then(|e| magicdab_dab::tables::country(e, (p.eid >> 12) as u8)).map(|c| c.0.to_string()),
            transmitters,
            recording: p.recording,
        };
        log::info!(
            "report of the new multiplex {} on {}: {} s, SNR {:.1} dB, {} transmitter(s) by TII",
            p.ev_eid,
            p.channel,
            report.seconds,
            report.snr_mean,
            report.transmitters.len()
        );
        let Some(ev) = self.changes.attach_report(p.time, &p.channel, &p.ev_eid, report) else { return };
        self.hub.broadcast_all(text(&json!({"type": "changeReport", "event": ev})));
        changes::discord_report(&self.settings.discord_webhook, &self.settings.station_name, &ev, p.discord);
    }
}
