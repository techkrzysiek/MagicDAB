//! Audio fan-out: encodes decoded 48 kHz stereo PCM of each service once
//! (Opus and/or 16 bit PCM) and sends it to the listening clients.
//!
//! Audio frame layout (binary WebSocket message):
//! `[0]=1 [1]=codec (1 Opus, 2 PCM s16le) [2..4]=sequence u16 LE [4..8]=SId u32 LE` + payload

use crate::hub::{Codec, Hub, Out, BIN_AUDIO};
use magicdab_codecs::opus::{OpusEncoder, FRAME};
use magicdab_dab::receiver::ServiceKey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Stream {
    opus: Option<OpusEncoder>,
    pending: Vec<f32>,
    seq: u16,
    /// Time spent encoding it (statistics).
    encoding: Duration,
}

pub struct AudioHub {
    streams: HashMap<ServiceKey, Stream>,
    bitrate: i32,
    /// The receiver whose listeners get the audio: one SId is often carried
    /// by two multiplexes (radio/slots.rs).
    slot: u32,
}

impl AudioHub {
    pub fn new(bitrate: u32) -> Self {
        AudioHub { streams: HashMap::new(), bitrate: bitrate as i32, slot: 0 }
    }

    /// The audio of a side slot.
    pub fn for_slot(bitrate: u32, slot: u32) -> Self {
        AudioHub { slot, ..Self::new(bitrate) }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) {
        self.bitrate = bitrate as i32;
        for s in self.streams.values_mut() {
            s.opus = None;
        }
    }

    pub fn remove(&mut self, key: &ServiceKey) {
        self.streams.remove(key);
    }

    pub fn clear(&mut self) {
        self.streams.clear();
    }

    /// Time spent encoding each service since its stream began.
    pub fn encoding_times(&self) -> impl Iterator<Item = (ServiceKey, Duration)> + '_ {
        self.streams.iter().map(|(k, s)| (*k, s.encoding))
    }

    fn header(codec: u8, seq: u16, sid: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + 4000);
        v.push(BIN_AUDIO);
        v.push(codec);
        v.extend_from_slice(&seq.to_le_bytes());
        v.extend_from_slice(&sid.to_le_bytes());
        v
    }

    pub fn push(&mut self, hub: &Hub, key: ServiceKey, pcm: &[f32], want_opus: bool, want_pcm: bool) {
        if !want_opus && !want_pcm {
            return;
        }
        let (bitrate, slot) = (self.bitrate, self.slot);
        let st = self.streams.entry(key).or_insert_with(|| Stream { opus: None, pending: Vec::new(), seq: 0, encoding: Duration::ZERO });
        st.pending.extend_from_slice(pcm);
        while st.pending.len() >= FRAME * 2 {
            let frame: Vec<f32> = st.pending.drain(..FRAME * 2).collect();
            st.seq = st.seq.wrapping_add(1);
            if want_opus {
                if st.opus.is_none() {
                    st.opus = OpusEncoder::new(bitrate).map_err(|e| log::error!("{e}")).ok();
                }
                if let Some(enc) = st.opus.as_mut() {
                    let t = Instant::now();
                    let pkt = enc.encode(&frame);
                    st.encoding += t.elapsed();
                    if let Some(pkt) = pkt {
                        let mut v = Self::header(1, st.seq, key.sid);
                        v.extend_from_slice(pkt);
                        let msg = Out::Binary(Arc::new(v));
                        hub.broadcast(msg, |c| c.audio && c.codec == Codec::Opus && c.service == Some(key) && c.slot == slot);
                    }
                }
            }
            if want_pcm {
                let mut v = Self::header(2, st.seq, key.sid);
                for s in &frame {
                    v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                let msg = Out::Binary(Arc::new(v));
                hub.broadcast(msg, |c| c.audio && c.codec == Codec::Pcm && c.service == Some(key) && c.slot == slot);
            }
        }
    }
}
