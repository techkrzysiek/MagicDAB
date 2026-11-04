//! Statistics for admins (the Statistics view): the listeners, the bandwidth,
//! the computer, and where the processor time of the receivers goes.

use super::*;
use crate::sysstats::{Machine, Sampler};
use magicdab_dab::receiver::Timing;

/// Samples while an admin looks at the statistics, and less often otherwise
/// (the history of the last hour).
const OFTEN: Duration = Duration::from_secs(2);
const SELDOM: Duration = Duration::from_secs(10);
const HISTORY: Duration = Duration::from_secs(3600);

pub struct State {
    sampler: Sampler,
    machine: Option<Machine>,
    started: Instant,
    last: Option<Instant>,
    /// Totals at the last sample, to take the differences from.
    prev: Prev,
    /// (unix time, CPU of the computer %, CPU of the program % of one
    /// processor, bandwidth out in bytes/s), the last hour.
    history: VecDeque<(i64, Option<f32>, Option<f32>, f64)>,
    /// Admins that got the history already.
    told: HashSet<u64>,
}

#[derive(Default)]
struct Prev {
    timing: HashMap<u32, Timing>,
    opus: HashMap<(u32, ServiceKey), Duration>,
    sent: u64,
    clients: HashMap<u64, u64>,
    inputs: HashMap<String, u64>,
}

impl State {
    pub fn new() -> Self {
        State {
            sampler: Sampler::default(),
            machine: None,
            started: Instant::now(),
            last: None,
            prev: Prev::default(),
            history: VecDeque::new(),
            told: HashSet::new(),
        }
    }
}

/// % of one processor of a time spent over `wall` seconds.
fn pct(now: Duration, before: Duration, wall: f32) -> f32 {
    now.saturating_sub(before).as_secs_f32() / wall * 100.0
}

fn r1(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}

impl Radio {
    /// From the tick: a sample when due, sent to the admins who look.
    pub(super) fn stats_tick(&mut self) {
        let watched = self.hub.any(|c| c.admin && c.subs.stats);
        let every = if watched { OFTEN } else { SELDOM };
        if self.stats.last.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        let msg = self.stats_sample(watched);
        if !watched {
            self.stats.told.clear();
            return;
        }
        // the history once to each admin who opens the view, then the new point
        let ids: Vec<u64> =
            self.hub.ids().into_iter().filter(|&id| self.hub.update(id, |c| c.admin && c.subs.stats).unwrap_or(false)).collect();
        self.stats.told.retain(|id| ids.contains(id));
        let history: Vec<Value> =
            self.stats.history.iter().map(|(t, cpu, prog, out)| json!([t, cpu.map(r1), prog.map(r1), out.round()])).collect();
        for id in ids {
            let mut m = msg.clone();
            if self.stats.told.insert(id) {
                m["history"] = json!(history);
            }
            self.hub.send(id, text(&m));
        }
    }

    fn stats_sample(&mut self, detailed: bool) -> Value {
        let now = Instant::now();
        let wall = self.stats.last.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0);
        // (after a long pause the differences say little: a new start)
        let fresh = wall > 0.0 && wall < 30.0;
        self.stats.last = Some(now);
        let sys = self.stats.sampler.sample();
        let machine = self.stats.machine.get_or_insert_with(crate::sysstats::machine).clone();

        // bandwidth: what the connections wrote, through the tunnel or not
        let sent = self.hub.sent.load(Ordering::Relaxed);
        let per_client = self.hub.sent_by_client();
        let (mut tunnel, mut local) = (0u64, 0u64);
        let mut clients = HashMap::new();
        for (id, bytes, via_tunnel) in per_client {
            let before = self.stats.prev.clients.get(&id).copied().unwrap_or(bytes);
            let d = bytes.saturating_sub(before);
            if via_tunnel {
                tunnel += d;
            } else {
                local += d;
            }
            clients.insert(id, bytes);
        }
        let rate = |bytes: u64| if fresh { bytes as f64 / wall as f64 } else { 0.0 };
        let out = rate(sent.saturating_sub(self.stats.prev.sent));
        self.stats.prev.sent = sent;
        self.stats.prev.clients = clients;

        let unix = chrono::Utc::now().timestamp();
        if fresh {
            self.stats.history.push_back((unix, sys.cpu, sys.process_cpu, out));
        }
        while self.stats.history.front().is_some_and(|h| unix - h.0 > HISTORY.as_secs() as i64) {
            self.stats.history.pop_front();
        }

        // the listeners
        let list = self.hub.list();
        let listening = list.iter().filter(|c| c.audio && c.service.is_some()).count();
        let mut stations: HashMap<(u32, u32), usize> = HashMap::new();
        for c in list.iter().filter(|c| c.audio) {
            if let Some(sid) = c.service {
                *stations.entry((c.slot, sid)).or_default() += 1;
            }
        }

        let mut msg = json!({
            "type": "serverStats",
            "uptime": self.stats.started.elapsed().as_secs(),
            "version": VERSION,
            "machine": machine,
            "system": {"cpu": sys.cpu.map(r1), "ramTotal": sys.ram_total, "ramUsed": sys.ram_used, "uptime": crate::sysstats::uptime()},
            "process": {"cpu": sys.process_cpu.map(r1), "ram": sys.process_ram, "threads": sys.thread_count},
            "listeners": {"connected": list.len(), "listening": listening},
            "net": {"out": out.round(), "sent": sent},
        });
        if !detailed {
            return msg;
        }

        // where the receivers' time goes: the main one, then the side slots
        let mut receivers = Vec::new();
        let mut inputs = Vec::new();
        let mut prev_timing = HashMap::new();
        let mut prev_opus = HashMap::new();
        let mut prev_inputs = HashMap::new();
        let mut add_input = |name: String, bytes: u64, prev: &HashMap<String, u64>, inputs: &mut Vec<Value>| {
            let d = bytes.saturating_sub(prev.get(&name).copied().unwrap_or(bytes));
            inputs.push(json!({"name": name, "rate": rate(d).round()}));
            prev_inputs.insert(name, bytes);
        };
        let main_channel = self.channel.clone().unwrap_or_default();
        let device = self.device.status();
        let main_input = if self.eti_input.is_some() {
            "stream"
        } else if self.dongle_active() {
            "dongle"
        } else {
            match device.kind.as_str() {
                "rtltcp" => "rtltcp",
                "rawfile" | "file" => "recording",
                "none" => "none",
                _ => "rtlsdr",
            }
        };
        if let Some(e) = &self.eti_input {
            add_input(format!("{main_channel} EDI/ETI"), e.bytes.load(Ordering::Relaxed), &self.stats.prev.inputs, &mut inputs);
        }
        if let Some(r) = device.stream_rate.filter(|_| main_input == "rtltcp") {
            // (two bytes a sample)
            inputs.push(json!({"name": format!("{main_channel} rtl_tcp"), "rate": (r as f64 * 2000.0).round()}));
        }
        let main = receiver_json(
            0,
            &main_channel,
            main_input,
            &self.rx.timing.lock(),
            &self.audio,
            &self.ensemble,
            &stations,
            fresh.then_some(wall),
            &self.stats.prev,
            &mut prev_timing,
            &mut prev_opus,
        );
        receivers.push(main);
        for e in self.slots.iter() {
            let (rx, audio, bytes, ensemble) = e.slot.stats_parts();
            let kind = if bytes.is_some() { "stream" } else { "recording" };
            if let Some(b) = bytes {
                add_input(format!("{} EDI/ETI", e.channel), b, &self.stats.prev.inputs, &mut inputs);
            }
            let timing = rx.timing.lock().clone();
            receivers.push(receiver_json(
                e.id,
                &e.channel,
                kind,
                &timing,
                audio,
                ensemble,
                &stations,
                fresh.then_some(wall),
                &self.stats.prev,
                &mut prev_timing,
                &mut prev_opus,
            ));
        }
        self.stats.prev.timing = prev_timing;
        self.stats.prev.opus = prev_opus;
        self.stats.prev.inputs = prev_inputs;

        let codec_pcm = list.iter().filter(|c| c.audio && c.codec == hub::Codec::Pcm).count();
        msg["listeners"]["admins"] = json!(list.iter().filter(|c| c.admin).count());
        msg["listeners"]["pcm"] = json!(codec_pcm);
        msg["net"]["tunnel"] = json!(rate(tunnel).round());
        msg["net"]["local"] = json!(rate(local).round());
        msg["net"]["inputs"] = json!(inputs);
        msg["receivers"] = json!(receivers);
        msg["threads"] = json!(sys.threads.iter().map(|t| json!({"name": t.name, "count": t.count, "cpu": r1(t.cpu)})).collect::<Vec<_>>());
        msg
    }
}

/// One receiver: its parts in % of one processor, the stations it decodes
/// and how many listen to each.
fn receiver_json(
    slot: u32,
    channel: &str,
    input: &str,
    t: &Timing,
    audio: &AudioHub,
    ensemble: &Ensemble,
    stations: &HashMap<(u32, u32), usize>,
    wall: Option<f32>,
    prev: &Prev,
    next_timing: &mut HashMap<u32, Timing>,
    next_opus: &mut HashMap<(u32, ServiceKey), Duration>,
) -> Value {
    let opus: HashMap<ServiceKey, Duration> = audio.encoding_times().collect();
    for (k, d) in &opus {
        next_opus.insert((slot, *k), *d);
    }
    next_timing.insert(slot, t.clone());
    let p = prev.timing.get(&slot);
    let (Some(wall), Some(p)) = (wall, p) else {
        return json!({"slot": slot, "channel": channel, "input": input});
    };
    let d = |now: Duration, before: Duration| pct(now, before, wall);
    let mut services = Vec::new();
    let mut parts = 0.0;
    for (key, (chan, dec)) in &t.services {
        let (c0, d0) = p.services.get(key).copied().unwrap_or((*chan, *dec));
        let (c, a) = (d(*chan, c0), d(*dec, d0));
        let o = opus.get(key).map(|now| d(*now, prev.opus.get(&(slot, *key)).copied().unwrap_or(*now))).unwrap_or(0.0);
        parts += c + a;
        // (no longer decoded: nothing new)
        if c + a + o == 0.0 {
            continue;
        }
        services.push(json!({
            "sid": key.sid,
            "label": service_label(ensemble, key.sid),
            "channel": r1(c),
            "audio": r1(a),
            "opus": r1(o),
            "listeners": stations.get(&(slot, key.sid)).copied().unwrap_or(0),
        }));
    }
    services.sort_by(|a, b| b["listeners"].as_u64().cmp(&a["listeners"].as_u64()));
    let (fic, background, data, tii) = (d(t.fic, p.fic), d(t.background, p.background), d(t.data, p.data), d(t.tii, p.tii));
    let recording = d(t.recording, p.recording);
    let thread = d(t.thread, p.thread);
    // what the thread did besides the parts measured: the OFDM demodulation
    // (the parts are measured on the clock: on a busy computer they come out
    // a little longer than their processor time)
    let ofdm = (thread - fic - parts - background - data - tii - recording).max(0.0);
    json!({
        "slot": slot,
        "channel": channel,
        "input": input,
        "cpu": r1(thread),
        "ofdm": r1(if input == "recording" || input == "rtlsdr" || input == "rtltcp" { ofdm } else { 0.0 }),
        "fic": r1(fic),
        "background": r1(background),
        "data": r1(data),
        "tii": r1(tii),
        // the DX recording (radio/dxrec.rs): the sub-channels nothing else decodes
        "recording": r1(recording),
        "services": services,
    })
}
