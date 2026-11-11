// The program's own sound output (crates/server/src/speaker.rs): a page on the
// same computer (the Android app, the Windows window, a browser there) leaves
// the sound to the program instead of decoding and playing it itself, which
// spares a phone a third of a core.
import { useStore, load } from '../store'
import type { Store } from '../store'
import { defaultBuffer, player } from './player'

/** The program plays the sound (it can here, and it was not turned off). */
export function nativeAudio(s: Store = useStore.getState()): boolean {
  return !!s.hello?.speaker && s.nativeAudio
}

/** The codec asked of the server. */
export function audioCodec(s: Store = useStore.getState()): 'opus' | 'pcm' | 'native' {
  return nativeAudio(s) ? 'native' : s.codec
}

/** The volume and the buffer for the program's output. */
export function speakerSettings() {
  const s = useStore.getState()
  return { type: 'speaker', gain: s.muted ? 0 : s.volume * s.volume, target: load('buffer', defaultBuffer()) }
}

const db = (v: number | null | undefined) => (v == null ? -Infinity : v)

/** The output's state, from the server five times a second. */
export function onSpeaker(m: {
  buffer: number
  target: number
  playing: boolean
  underruns: number
  meter: { m: number | null; s: number | null; i: number | null; peak: number | null; max: number | null } | null
  error: string | null
}) {
  const st = useStore.getState()
  if (!st.audioEnabled || !nativeAudio(st)) return
  const meter = m.meter ? { m: db(m.meter.m), s: db(m.meter.s), i: db(m.meter.i), peak: db(m.meter.peak), max: db(m.meter.max) } : st.meter
  // (the level bar and the silence note go by the player's peak)
  if (m.meter?.peak != null) player.peak = Math.max(player.peak, 10 ** (m.meter.peak / 20))
  st.set({
    audioBuffer: m.buffer,
    audioTarget: m.target,
    audioPlaying: m.playing,
    audioUnderruns: m.underruns,
    meter,
    speakerError: m.error ?? null,
  })
}
