// Audio playback of the WebSocket stream (Opus via WebCodecs or WASM, or raw PCM)
// through an AudioWorklet ring buffer with drift compensation.
import { useStore, load, isLocal } from '../store'
import { recordOpus } from './recorder'

const WORKLET = `
class MagicDabPlayer extends AudioWorkletProcessor {
  constructor() {
    super()
    this.size = 48000 * 2 * 6
    this.buf = new Float32Array(this.size)
    this.r = 0
    this.w = 0
    this.count = 0
    this.playing = false
    this.target = 48000 * 0.35
    this.tick = 0
    this.fade = 0
    this.underruns = 0
    this.port.onmessage = (e) => {
      const d = e.data
      if (d === 'flush') { this.r = this.w = this.count = 0; this.playing = false; return }
      if (d && d.target) { this.target = d.target; return }
      const n = d.length >> 1
      if (this.count + n > this.size / 2) {
        // overflow: drop the oldest audio
        const drop = this.count + n - this.size / 2
        this.r = (this.r + drop * 2) % this.size
        this.count -= drop
      }
      for (let i = 0; i < d.length; i++) {
        this.buf[this.w] = d[i]
        this.w = (this.w + 1) % this.size
      }
      this.count += n
    }
  }
  process(inputs, outputs) {
    const out = outputs[0]
    const L = out[0], R = out[1] || out[0]
    const n = L.length
    if (++this.tick % 100 === 0) this.port.postMessage({ level: this.count / 48000, playing: this.playing, underruns: this.underruns })
    if (!this.playing) {
      if (this.count >= this.target) { this.playing = true; this.fade = 0 }
      else { L.fill(0); if (R !== L) R.fill(0); return true }
    }
    if (this.count < n) { this.playing = false; this.underruns++; L.fill(0); if (R !== L) R.fill(0); return true }
    // drift compensation: skip or repeat one sample frame per quantum
    let skip = 0, repeat = false
    if (this.count > this.target * 2 + 4800) skip = 1
    else if (this.count < this.target * 0.5 && this.count > n * 2) repeat = true
    for (let i = 0; i < n; i++) {
      if (repeat && i === n >> 1) {
        const pr = (this.r - 2 + this.size) % this.size
        L[i] = this.buf[pr]; R[i] = this.buf[pr + 1]
        continue
      }
      let g = 1
      if (this.fade < 2048) { g = this.fade / 2048; this.fade++ }
      L[i] = this.buf[this.r] * g
      R[i] = this.buf[this.r + 1] * g
      this.r = (this.r + 2) % this.size
      this.count--
    }
    if (skip) { this.r = (this.r + 2) % this.size; this.count-- }
    return true
  }
}
registerProcessor('magicdab-player', MagicDabPlayer)

// Loudness as in broadcasting (ITU-R BS.1770 / EBU R128): K-weighted mean
// square of both channels over 400 ms (momentary), 3 s (short-term) and since
// the station was selected (integrated, gated at -70 LUFS and 10 LU below the
// mean), in LUFS, and the sample peak in dBFS (also its maximum since then);
// reported every 100 ms (48 kHz). A 'reset' message starts anew.
class MagicDabMeter extends AudioWorkletProcessor {
  constructor() {
    super()
    this.port.onmessage = (e) => { if (e.data === 'reset') this.reset() }
    this.reset()
    // K-weighting: shelf (head) and high-pass (RLB) for 48 kHz
    this.f = [[1.53512485958697, -2.69169618940638, 1.19839281085285, -1.69065929318241, 0.73248077421585],
              [1.0, -2.0, 1.0, -1.99004745483398, 0.99007225036621]]
    this.z = [[0, 0, 0, 0], [0, 0, 0, 0]].map(() => [[0, 0], [0, 0]])
  }
  reset() {
    this.blocks = []
    this.sum = 0
    this.n = 0
    this.peak = 0
    this.max = 0
    // 400 ms blocks above -70 LUFS: histogram of 0.1 LU from -70 to +5 (count, power)
    this.hist = new Float64Array(750)
    this.histP = new Float64Array(750)
  }
  integrated() {
    let n = 0, p = 0
    for (let i = 0; i < 750; i++) { n += this.hist[i]; p += this.histP[i] }
    if (!n) return -Infinity
    const gate = -0.691 + 10 * Math.log10(p / n) - 10
    let n2 = 0, p2 = 0
    for (let i = 0; i < 750; i++) {
      if (-70 + i / 10 >= gate) { n2 += this.hist[i]; p2 += this.histP[i] }
    }
    return n2 ? -0.691 + 10 * Math.log10(p2 / n2) : -Infinity
  }
  kw(ch, x) {
    for (let s = 0; s < 2; s++) {
      const [b0, b1, b2, a1, a2] = this.f[s]
      const z = this.z[ch][s]
      const y = b0 * x + z[0]
      z[0] = b1 * x - a1 * y + z[1]
      z[1] = b2 * x - a2 * y
      x = y
    }
    return x
  }
  process(inputs) {
    const input = inputs[0]
    if (input && input.length) {
      const chans = Math.min(2, input.length)
      const len = input[0].length
      for (let c = 0; c < chans; c++) {
        const d = input[c]
        for (let i = 0; i < len; i++) {
          const a = Math.abs(d[i])
          if (a > this.peak) this.peak = a
          if (a > this.max) this.max = a
          const y = this.kw(c, d[i])
          this.sum += y * y
        }
      }
      // mono counts as both channels
      if (chans === 1) this.sum *= 2
      this.n += len
    }
    if (this.n >= 4800) {
      this.blocks.push(this.sum / this.n)
      if (this.blocks.length > 30) this.blocks.shift()
      const lufs = (k) => {
        const b = this.blocks.slice(-k)
        const ms = b.reduce((x, y) => x + y, 0) / b.length
        return ms > 0 ? -0.691 + 10 * Math.log10(ms) : -Infinity
      }
      const m = lufs(4)
      // a gating block every 100 ms (400 ms long, 75 % overlap)
      if (this.blocks.length >= 4 && m >= -70) {
        const i = Math.min(749, Math.floor((m + 70) * 10))
        this.hist[i] += 1
        this.histP[i] += Math.pow(10, (m + 0.691) / 10)
      }
      const db = (x) => (x > 0 ? 20 * Math.log10(x) : -Infinity)
      this.port.postMessage({ m, s: lufs(30), i: this.integrated(), peak: db(this.peak), max: db(this.max) })
      this.sum = 0
      this.n = 0
      this.peak = 0
    }
    return true
  }
}
registerProcessor('magicdab-meter', MagicDabMeter)
`

type Decoder = { decode(data: Uint8Array): void; close(): void }

/**
 * Fallback output for insecure contexts (plain http on a LAN IP) and browsers
 * without AudioWorklet: decoded chunks are scheduled as AudioBufferSourceNodes.
 */
class BufferOutput {
  next = 0
  target = 0.35
  fade = 0
  underruns = 0
  started = false
  private sources = new Set<AudioBufferSourceNode>()
  constructor(private ctx: AudioContext, private out: AudioNode) {}

  push(interleaved: Float32Array) {
    const n = interleaved.length >> 1
    if (n === 0) return
    const buf = this.ctx.createBuffer(2, n, 48000)
    const l = buf.getChannelData(0)
    const r = buf.getChannelData(1)
    for (let i = 0; i < n; i++) {
      l[i] = interleaved[2 * i]
      r[i] = interleaved[2 * i + 1]
    }
    const now = this.ctx.currentTime
    if (this.next < now + 0.02) {
      // underrun / start: build up the buffer again and fade in
      if (this.started) this.underruns++
      this.started = true
      this.next = now + this.target
      this.fade = 0
    }
    if (this.fade < 2048) {
      for (let i = 0; i < n && this.fade < 2048; i++, this.fade++) {
        const g = this.fade / 2048
        l[i] *= g
        r[i] *= g
      }
    }
    if (this.next - now > this.target + 1.5 && this.fade >= 2048) {
      // too much latency accumulated (clock drift / burst): drop this chunk
      return
    }
    const src = this.ctx.createBufferSource()
    src.buffer = buf
    src.connect(this.out)
    src.start(this.next)
    this.sources.add(src)
    src.onended = () => this.sources.delete(src)
    this.next += n / 48000
  }

  level() {
    return Math.max(0, this.next - this.ctx.currentTime)
  }

  flush() {
    // what was scheduled would still play (the old station after a switch)
    for (const s of this.sources) s.stop()
    this.sources.clear()
    this.next = 0
    this.started = false
  }
}

/** The buffer of a browser that chose none: more over the internet (a tunnel
 *  and other networks between) than in the same network. */
export const defaultBuffer = () => (isLocal() ? 0.3 : 0.5)

class Player {
  ctx: AudioContext | null = null
  node: AudioWorkletNode | null = null
  fallback: BufferOutput | null = null
  gain: GainNode | null = null
  /** the programme before the volume control (the meter listens here) */
  pre: GainNode | null = null
  meter: AudioWorkletNode | null = null
  decoder: Decoder | null = null
  decoderKind: 'webcodecs' | 'wasm' | null = null
  currentSid: number | null = null
  ts = 0
  starting: Promise<void> | null = null
  levelTimer: number | null = null
  /** The buffer chosen (Settings), what dropouts added to it, and when it
   *  last changed (adapt). */
  private base = 0.35
  private extra = 0
  private seenUnderruns = 0
  private changedAt = 0

  async start() {
    if (this.starting) return this.starting
    this.starting = this.doStart()
    try {
      await this.starting
    } finally {
      this.starting = null
    }
  }

  private async doStart() {
    if (!this.ctx) {
      const AC = window.AudioContext || (window as any).webkitAudioContext
      let ctx: AudioContext
      try {
        ctx = new AC({ sampleRate: 48000, latencyHint: 'playback' })
      } catch {
        ctx = new AC()
      }
      this.ctx = ctx
      // resume inside the user gesture (required by iOS / mobile browsers)
      ctx.resume().catch(() => undefined)
      this.keepRunning(ctx)
      this.gain = ctx.createGain()
      this.gain.connect(ctx.destination)
      this.pre = ctx.createGain()
      this.pre.connect(this.gain)
      this.applyVolume()
      const canWorklet = !!ctx.audioWorklet && window.isSecureContext && ctx.sampleRate === 48000
      if (canWorklet) {
        try {
          const url = URL.createObjectURL(new Blob([WORKLET], { type: 'application/javascript' }))
          // some mobile browsers reject (or never settle) worklet modules from blob URLs
          await Promise.race([
            ctx.audioWorklet.addModule(url),
            new Promise((_, reject) => setTimeout(() => reject(new Error('addModule timeout')), 3000)),
          ])
          this.node = new AudioWorkletNode(ctx, 'magicdab-player', { outputChannelCount: [2] })
          this.node.port.onmessage = (e) => {
            if (e.data && typeof e.data.level === 'number') {
              useStore.getState().set({ audioBuffer: e.data.level, audioPlaying: e.data.playing, audioUnderruns: e.data.underruns ?? 0 })
              this.adapt(e.data.underruns ?? 0)
            }
          }
          this.node.connect(this.pre)
          this.startMeter(ctx)
        } catch (e) {
          console.warn('AudioWorklet unavailable, using buffer scheduling', e)
          this.node = null
        }
      }
      if (!this.node) {
        this.fallback = new BufferOutput(ctx, this.pre)
        this.levelTimer = window.setInterval(() => {
          const lvl = this.fallback?.level() ?? 0
          useStore.getState().set({ audioBuffer: lvl, audioPlaying: lvl > 0.02, audioUnderruns: this.fallback?.underruns ?? 0 })
          this.adapt(this.fallback?.underruns ?? 0)
        }, 500)
      }
      // the buffer chosen in Settings (kept in this browser)
      this.setTarget(load('buffer', defaultBuffer()))
    }
    if (this.ctx.state !== 'running') await this.ctx.resume()
    if (!this.decoder) await this.createDecoder()
  }

  peak = 0

  /**
   * Browsers suspend (or "interrupt" on iOS) the audio context when the page
   * goes to the background, the screen locks or the output device changes;
   * resume it as soon as possible while playback is on.
   */
  /** Loudness meter on the programme (LUFS, peak), reported to the store. */
  private startMeter(ctx: AudioContext) {
    try {
      this.meter = new AudioWorkletNode(ctx, 'magicdab-meter', { outputChannelCount: [1] })
      this.pre?.connect(this.meter)
      // processed only when it leads somewhere: a silent way to the speakers
      const silent = ctx.createGain()
      silent.gain.value = 0
      this.meter.connect(silent)
      silent.connect(ctx.destination)
      this.meter.port.onmessage = (e) => {
        const { m, s, i, peak, max } = e.data as { m: number; s: number; i: number; peak: number; max: number }
        useStore.getState().set({ meter: { m, s, i, peak, max } })
      }
    } catch (e) {
      console.warn('loudness meter unavailable', e)
    }
  }

  private keepRunning(ctx: AudioContext) {
    const resume = () => {
      if (!useStore.getState().audioEnabled || ctx.state === 'running' || ctx.state === 'closed') return
      ctx.resume().catch(() => undefined)
    }
    ctx.addEventListener('statechange', resume)
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible') resume()
    })
    window.addEventListener('pageshow', resume)
    window.addEventListener('focus', resume)
  }

  private post(interleaved: Float32Array) {
    let p = 0
    for (let i = 0; i < interleaved.length; i += 4) {
      const v = Math.abs(interleaved[i])
      if (v > p) p = v
    }
    this.peak = Math.max(p, this.peak * 0.9)
    if (this.node) this.node.port.postMessage(interleaved, [interleaved.buffer])
    else this.fallback?.push(interleaved)
  }

  private async createDecoder() {
    const post = (interleaved: Float32Array) => this.post(interleaved)
    const AD = window.isSecureContext ? (window as any).AudioDecoder : undefined
    if (AD) {
      try {
        const cfg = { codec: 'opus', sampleRate: 48000, numberOfChannels: 2 }
        const sup = await AD.isConfigSupported(cfg)
        if (sup.supported) {
          const dec = new AD({
            output: (ad: any) => {
              const frames = ad.numberOfFrames
              const ch = ad.numberOfChannels
              const out = new Float32Array(frames * 2)
              if (ad.format === 'f32-planar') {
                const l = new Float32Array(frames)
                const r = new Float32Array(frames)
                ad.copyTo(l, { planeIndex: 0 })
                if (ch > 1) ad.copyTo(r, { planeIndex: 1 })
                else r.set(l)
                for (let i = 0; i < frames; i++) {
                  out[2 * i] = l[i]
                  out[2 * i + 1] = r[i]
                }
              } else {
                const tmp = new Float32Array(frames * ch)
                ad.copyTo(tmp, { planeIndex: 0, format: 'f32' })
                for (let i = 0; i < frames; i++) {
                  out[2 * i] = tmp[i * ch]
                  out[2 * i + 1] = tmp[i * ch + (ch > 1 ? 1 : 0)]
                }
              }
              ad.close()
              post(out)
            },
            error: (e: any) => {
              // a closed decoder never decodes again: start a new one
              console.warn('AudioDecoder error', e)
              this.decoder = null
              this.decoderKind = null
              this.createDecoder().catch(() => undefined)
            },
          })
          dec.configure(cfg)
          this.decoder = {
            decode: (data) => {
              if (dec.state !== 'configured') return
              this.ts += 20000
              dec.decode(new (window as any).EncodedAudioChunk({ type: 'key', timestamp: this.ts, data }))
            },
            close: () => dec.close(),
          }
          this.decoderKind = 'webcodecs'
          return
        }
      } catch (e) {
        console.warn('WebCodecs opus not available', e)
      }
    }
    const { OpusDecoder } = await import('opus-decoder')
    const dec = new OpusDecoder({ channels: 2 })
    await dec.ready
    this.decoder = {
      decode: (data) => {
        const r = dec.decodeFrame(data)
        const n = r.samplesDecoded
        const out = new Float32Array(n * 2)
        const l = r.channelData[0]
        const rr = r.channelData[1] || l
        for (let i = 0; i < n; i++) {
          out[2 * i] = l[i]
          out[2 * i + 1] = rr[i]
        }
        post(out)
      },
      close: () => dec.free(),
    }
    this.decoderKind = 'wasm'
  }

  /** Called for every binary audio message. */
  packet(b: Uint8Array) {
    if ((!this.node && !this.fallback) || !this.ctx) return
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength)
    const codec = b[1]
    const sid = dv.getUint32(4, true)
    const sel = useStore.getState().selected
    if (!sel || sel.sid !== sid) return
    if (this.currentSid !== sid) {
      this.currentSid = sid
      this.flush()
    }
    const payload = b.subarray(8)
    if (codec === 1) {
      recordOpus(sid, payload)
      this.decoder?.decode(payload)
    } else if (codec === 2) {
      const n = payload.byteLength >> 1
      const out = new Float32Array(n)
      const pv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength)
      for (let i = 0; i < n; i++) out[i] = pv.getInt16(2 * i, true) / 32768
      this.post(out)
    }
  }

  flush() {
    // the meter starts anew with the next station
    this.meter?.port.postMessage('reset')
    this.node?.port.postMessage('flush')
    this.fallback?.flush()
  }

  /** The buffer chosen; dropouts may add to it for a while (adapt). */
  setTarget(seconds: number) {
    this.base = seconds
    this.applyTarget()
  }

  /**
   * A dropout makes the buffer a quarter of a second longer (up to 2 s in
   * all): the network or the server was late, and is likely to be again;
   * after 10 quiet minutes it gets a quarter shorter again, back to the
   * buffer chosen.
   */
  private adapt(underruns: number) {
    const now = Date.now()
    if (underruns > this.seenUnderruns) {
      this.extra = Math.min(this.extra + 0.25, Math.max(0, 2 - this.base))
      this.changedAt = now
      this.applyTarget()
    } else if (this.extra > 0 && now - this.changedAt > 10 * 60_000) {
      this.extra = Math.max(0, this.extra - 0.25)
      this.changedAt = now
      this.applyTarget()
    }
    this.seenUnderruns = underruns
  }

  private applyTarget() {
    const seconds = this.base + this.extra
    this.node?.port.postMessage({ target: Math.round(seconds * 48000) })
    if (this.fallback) this.fallback.target = seconds
    useStore.getState().set({ audioTarget: seconds })
  }

  applyVolume() {
    const s = useStore.getState()
    if (this.gain) this.gain.gain.value = s.muted ? 0 : s.volume * s.volume
  }

  async suspend() {
    this.flush()
    if (this.ctx && this.ctx.state === 'running') await this.ctx.suspend()
  }
}

export const player = new Player()
