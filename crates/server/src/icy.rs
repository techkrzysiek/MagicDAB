//! Icecast style streams of the stations: `GET /stream/<SId>` gives a station
//! of the multiplex received as Ogg Opus over HTTP, with its dynamic label
//! (DLS) as the ICY `StreamTitle` for players that ask for it
//! (`Icy-MetaData: 1`). A station is decoded and encoded only while somebody
//! listens to its stream. Which stations, and how many at once, the radio
//! decides (settings `streams`, a receiver locked to one station, the
//! dongle's single sub-channel).

use crate::clips::{opus_headers, Ogg, PRE_SKIP};
use magicdab_codecs::opus::FRAME;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;

/// Packets (20 ms) a listener may fall behind before it skips ahead.
const BACKLOG: usize = 250;
/// Bytes of audio between two ICY metadata blocks.
pub const META_INTERVAL: usize = 16_000;
/// Packets per Ogg page (0.2 s).
const PACKETS_PER_PAGE: usize = 10;

/// What may be streamed, set by the radio thread.
#[derive(Clone, Default)]
pub struct Policy {
    pub enabled: bool,
    /// Stations streamed at once.
    pub max: usize,
    /// The stations of the multiplex received: SId and name.
    pub stations: HashMap<u32, String>,
    /// Only this station (a receiver locked to it, a dongle receiving it).
    pub only: Option<u32>,
    pub channel: String,
    /// kbps
    pub bitrate: u32,
}

struct Station {
    listeners: usize,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
}

#[derive(Default)]
struct Inner {
    policy: Policy,
    stations: HashMap<u32, Station>,
    titles: HashMap<u32, String>,
}

#[derive(Default)]
pub struct Streams {
    inner: Mutex<Inner>,
    changed: AtomicBool,
}

#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// Streams are off on this server.
    Off,
    /// No such station on the multiplex received, or not the one allowed.
    Unknown,
    /// As many stations as allowed are streamed already.
    Busy,
}

impl Streams {
    pub fn set_policy(&self, p: Policy) {
        self.inner.lock().policy = p;
    }

    /// A listener of station `sid`, or why not.
    pub fn subscribe(self: &Arc<Self>, sid: u32) -> Result<Subscription, Refusal> {
        let mut g = self.inner.lock();
        let p = &g.policy;
        if !p.enabled {
            return Err(Refusal::Off);
        }
        let Some(name) = p.stations.get(&sid).cloned().filter(|_| p.only.is_none_or(|o| o == sid)) else {
            return Err(Refusal::Unknown);
        };
        let (max, channel, bitrate) = (p.max.max(1), p.channel.clone(), p.bitrate);
        if !g.stations.contains_key(&sid) && g.stations.len() >= max {
            return Err(Refusal::Busy);
        }
        let st = g.stations.entry(sid).or_insert_with(|| Station { listeners: 0, tx: broadcast::channel(BACKLOG).0 });
        st.listeners += 1;
        let rx = st.tx.subscribe();
        drop(g);
        self.changed.store(true, Ordering::Relaxed);
        Ok(Subscription { streams: self.clone(), sid, rx, name, channel, bitrate, since: Instant::now() })
    }

    /// The stations listened to (radio thread).
    pub fn wanted(&self) -> HashSet<u32> {
        self.inner.lock().stations.keys().copied().collect()
    }

    /// Whether the stations listened to changed since the last call.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }

    /// Listeners of all the streams.
    #[cfg(test)]
    fn listeners(&self) -> usize {
        self.inner.lock().stations.values().map(|s| s.listeners).sum()
    }

    /// An Opus packet (20 ms, 48 kHz stereo) of station `sid` to its listeners.
    pub fn send(&self, sid: u32, packet: Vec<u8>) {
        if let Some(st) = self.inner.lock().stations.get(&sid) {
            let _ = st.tx.send(Arc::new(packet));
        }
    }

    /// The station's dynamic label, its stream title.
    pub fn set_title(&self, sid: u32, title: &str) {
        self.inner.lock().titles.insert(sid, title.trim().to_string());
    }

    fn title(&self, sid: u32) -> Option<String> {
        self.inner.lock().titles.get(&sid).filter(|t| !t.is_empty()).cloned()
    }
}

/// A listener of a stream; the last one leaving stops the station's decoding.
pub struct Subscription {
    streams: Arc<Streams>,
    pub sid: u32,
    rx: broadcast::Receiver<Arc<Vec<u8>>>,
    pub name: String,
    pub channel: String,
    pub bitrate: u32,
    since: Instant,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut g = self.streams.inner.lock();
        if let Some(st) = g.stations.get_mut(&self.sid) {
            st.listeners -= 1;
            if st.listeners == 0 {
                g.stations.remove(&self.sid);
            }
        }
        drop(g);
        self.streams.changed.store(true, Ordering::Relaxed);
        log::info!("stream: {} ({:04X}) ended after {} s", self.name, self.sid, self.since.elapsed().as_secs());
    }
}

/// The bytes one listener gets: the Ogg headers, then pages of the packets
/// as they come, with ICY metadata blocks between them when asked for.
pub struct Listener {
    sub: Subscription,
    ogg: Ogg,
    granule: u64,
    started: bool,
    meta: bool,
    until_meta: usize,
    sent_title: Option<String>,
}

impl Listener {
    pub fn new(sub: Subscription, meta: bool) -> Self {
        Listener { sub, ogg: Ogg::new(), granule: PRE_SKIP as u64, started: false, meta, until_meta: META_INTERVAL, sent_title: None }
    }

    /// The next part of the stream; `None` when the station is gone.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        if !self.started {
            self.started = true;
            let name = self.sub.name.clone();
            opus_headers(&mut self.ogg, &name);
        } else {
            let mut packets: Vec<Arc<Vec<u8>>> = Vec::with_capacity(PACKETS_PER_PAGE);
            while packets.len() < PACKETS_PER_PAGE {
                match self.sub.rx.recv().await {
                    Ok(p) => packets.push(p),
                    // (a listener too slow for the stream skips ahead)
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
            self.granule += (packets.len() * FRAME) as u64;
            let refs: Vec<&[u8]> = packets.iter().map(|p| p.as_slice()).collect();
            self.ogg.page(&refs, self.granule, 0);
        }
        let data = std::mem::take(&mut self.ogg.out);
        Some(self.frame(&data))
    }

    /// The audio bytes with a metadata block after every `META_INTERVAL` of them.
    fn frame(&mut self, mut data: &[u8]) -> Vec<u8> {
        if !self.meta {
            return data.to_vec();
        }
        let mut out = Vec::with_capacity(data.len() + 64);
        while !data.is_empty() {
            let n = self.until_meta.min(data.len());
            out.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.until_meta -= n;
            if self.until_meta == 0 {
                let block = self.meta_block();
                out.extend_from_slice(&block);
                self.until_meta = META_INTERVAL;
            }
        }
        out
    }

    /// `StreamTitle='…';` when the title changed (the DLS, else the station's
    /// name), else an empty block.
    fn meta_block(&mut self) -> Vec<u8> {
        let title = self.sub.streams.title(self.sub.sid).unwrap_or_else(|| self.sub.name.clone());
        if self.sent_title.as_deref() == Some(title.as_str()) {
            return vec![0];
        }
        let block = meta_text(&title);
        self.sent_title = Some(title);
        block
    }
}

/// An ICY metadata block: length in 16 byte units, then the text padded with zeros.
fn meta_text(title: &str) -> Vec<u8> {
    // (a quote would end the title early in the players' parsers)
    let mut t = title.replace('\'', "’");
    while t.len() > 255 * 16 - 16 {
        t.pop();
    }
    let text = format!("StreamTitle='{t}';");
    let units = text.len().div_ceil(16);
    let mut v = Vec::with_capacity(1 + units * 16);
    v.push(units as u8);
    v.extend_from_slice(text.as_bytes());
    v.resize(1 + units * 16, 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(stations: &[(u32, &str)], max: usize, only: Option<u32>) -> Policy {
        Policy {
            enabled: true,
            max,
            stations: stations.iter().map(|(s, n)| (*s, n.to_string())).collect(),
            only,
            channel: "5B".into(),
            bitrate: 128,
        }
    }

    #[test]
    fn stations_are_decoded_while_listened_to_and_as_many_as_allowed() {
        let s = Arc::new(Streams::default());
        assert_eq!(s.subscribe(0x4C7E).err(), Some(Refusal::Off));
        s.set_policy(policy(&[(0x4C7E, "Station A"), (0x5D21, "Station B"), (0x5D23, "Station C")], 2, None));
        let a = s.subscribe(0x4C7E).unwrap();
        let b = s.subscribe(0x4C7E).unwrap();
        let c = s.subscribe(0x5D21).unwrap();
        assert_eq!(s.subscribe(0x5D23).err(), Some(Refusal::Busy));
        assert_eq!(s.subscribe(0x1234).err(), Some(Refusal::Unknown));
        assert!(s.take_changed());
        assert_eq!(s.wanted(), HashSet::from([0x4C7E, 0x5D21]));
        assert_eq!(s.listeners(), 3);
        drop(a);
        assert_eq!(s.wanted().len(), 2);
        drop((b, c));
        assert!(s.wanted().is_empty() && s.take_changed());
    }

    #[test]
    fn a_locked_receiver_streams_its_station_only() {
        let s = Arc::new(Streams::default());
        s.set_policy(policy(&[(0x4C7E, "Station A"), (0x5D21, "Station B")], 4, Some(0x4C7E)));
        assert_eq!(s.subscribe(0x5D21).err(), Some(Refusal::Unknown));
        assert!(s.subscribe(0x4C7E).is_ok());
    }

    #[tokio::test]
    async fn the_stream_is_ogg_with_the_dls_between_its_bytes() {
        let s = Arc::new(Streams::default());
        s.set_policy(policy(&[(0x4C7E, "Station A")], 1, None));
        let mut l = Listener::new(s.subscribe(0x4C7E).unwrap(), true);
        let head = l.next_chunk().await.unwrap();
        assert!(head.starts_with(b"OggS") && head.windows(8).any(|w| w == b"OpusHead"));
        s.set_title(0x4C7E, "Artist - It's a song");
        let mut audio = head.len();
        let mut got = head;
        while audio < META_INTERVAL + 2000 {
            for _ in 0..PACKETS_PER_PAGE {
                s.send(0x4C7E, vec![0xFC; 300]);
            }
            let c = l.next_chunk().await.unwrap();
            audio += c.len();
            got.extend_from_slice(&c);
        }
        // the first block, after META_INTERVAL bytes of audio
        let len = got[META_INTERVAL] as usize * 16;
        let block = String::from_utf8_lossy(&got[META_INTERVAL + 1..META_INTERVAL + 1 + len]).trim_end_matches('\0').to_string();
        assert_eq!(block, "StreamTitle='Artist - It’s a song';");
        // without it, whole Ogg pages one after another
        let audio: Vec<u8> = [&got[..META_INTERVAL], &got[META_INTERVAL + 1 + len..]].concat();
        let (mut at, mut pages) = (0, 0);
        while at < audio.len() {
            assert_eq!(&audio[at..at + 4], b"OggS", "page {pages} at {at}");
            let segments = audio[at + 26] as usize;
            let body: usize = audio[at + 27..at + 27 + segments].iter().map(|&b| b as usize).sum();
            at += 27 + segments + body;
            pages += 1;
        }
        assert_eq!(at, audio.len());
        assert!(pages > 5);
    }
}
