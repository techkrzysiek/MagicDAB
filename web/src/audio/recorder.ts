// Client side recording: original DAB/DAB+ audio (LOAS AAC / MP2 frames sent by
// the server) or the received Opus stream stored in an Ogg container.
import { useStore } from '../store'
import { send } from '../ws'
import { downloadBlob } from '../util'

const CRC = (() => {
  const t = new Uint32Array(256)
  for (let i = 0; i < 256; i++) {
    let r = i << 24
    for (let j = 0; j < 8; j++) r = r & 0x80000000 ? (r << 1) ^ 0x04c11db7 : r << 1
    t[i] = r >>> 0
  }
  return t
})()

function crc32(d: Uint8Array) {
  let c = 0
  for (let i = 0; i < d.length; i++) c = ((c << 8) ^ CRC[((c >>> 24) ^ d[i]) & 0xff]) >>> 0
  return c >>> 0
}

class OggOpus {
  chunks: Uint8Array[] = []
  serial = (Math.random() * 0xffffffff) >>> 0
  seq = 0
  granule = 0
  pending: Uint8Array | null = null
  bytes = 0

  constructor(title: string) {
    const head = new Uint8Array(19)
    head.set(new TextEncoder().encode('OpusHead'), 0)
    const dv = new DataView(head.buffer)
    head[8] = 1
    head[9] = 2
    dv.setUint16(10, 312, true)
    dv.setUint32(12, 48000, true)
    this.page(head, 0x02, 0)
    const vendor = new TextEncoder().encode('MagicDAB')
    const comment = new TextEncoder().encode('TITLE=' + title)
    const tags = new Uint8Array(8 + 4 + vendor.length + 4 + 4 + comment.length)
    const tv = new DataView(tags.buffer)
    tags.set(new TextEncoder().encode('OpusTags'), 0)
    tv.setUint32(8, vendor.length, true)
    tags.set(vendor, 12)
    tv.setUint32(12 + vendor.length, 1, true)
    tv.setUint32(16 + vendor.length, comment.length, true)
    tags.set(comment, 20 + vendor.length)
    this.page(tags, 0, 0)
  }

  private page(packet: Uint8Array, type: number, granule: number) {
    const lacing: number[] = []
    let n = packet.length
    while (n >= 255) {
      lacing.push(255)
      n -= 255
    }
    lacing.push(n)
    const p = new Uint8Array(27 + lacing.length + packet.length)
    const dv = new DataView(p.buffer)
    p.set([0x4f, 0x67, 0x67, 0x53], 0)
    p[5] = type
    dv.setUint32(6, granule >>> 0, true)
    dv.setUint32(10, Math.floor(granule / 0x100000000), true)
    dv.setUint32(14, this.serial, true)
    dv.setUint32(18, this.seq++, true)
    p[26] = lacing.length
    p.set(lacing, 27)
    p.set(packet, 27 + lacing.length)
    dv.setUint32(22, crc32(p), true)
    this.chunks.push(p)
    this.bytes += p.length
  }

  packet(data: Uint8Array) {
    if (this.pending) this.page(this.pending, 0, this.granule)
    this.granule += 960
    this.pending = data.slice()
  }

  finish(): Blob {
    if (this.pending) this.page(this.pending, 0x04, this.granule)
    this.pending = null
    return new Blob(this.chunks as BlobPart[], { type: 'audio/ogg' })
  }
}

interface Active {
  format: 'aac' | 'opus'
  sid: number
  label: string
  started: number
  ogg?: OggOpus
  raw: Uint8Array[]
  aac: boolean
}

let active: Active | null = null

export function recordingActive() {
  return active
}

// a recording is of one station: choosing another one ends it (and saves it)
useStore.subscribe((s, prev) => {
  if (active && s.selected !== prev.selected && s.selected?.sid !== active.sid) stopRecording()
})

export function startRecording(format: 'aac' | 'opus') {
  const s = useStore.getState()
  const sid = s.selected?.sid
  if (!sid) return
  const label = s.ensemble.services?.find((x) => x.sid === sid)?.label ?? sid.toString(16)
  active = { format, sid, label, started: Date.now(), raw: [], aac: true, ogg: format === 'opus' ? new OggOpus(label) : undefined }
  if (format === 'aac') send({ type: 'record', enabled: true })
  s.set({ recording: { format, started: active.started, bytes: 0, label } })
}

export function stopRecording() {
  const a = active
  if (!a) return
  active = null
  if (a.format === 'aac') send({ type: 'record', enabled: false })
  const stamp = new Date(a.started).toISOString().replace(/[:T]/g, '-').slice(0, 19)
  const safe = a.label.replace(/[^\p{L}\p{N}_-]+/gu, '_')
  if (a.format === 'opus' && a.ogg) {
    downloadBlob(`${safe}_${stamp}.opus`, a.ogg.finish())
  } else {
    downloadBlob(`${safe}_${stamp}.${a.aac ? 'aac' : 'mp2'}`, new Blob(a.raw as BlobPart[], { type: a.aac ? 'audio/aac' : 'audio/mpeg' }))
  }
  useStore.getState().set({ recording: null })
}

function bump(n: number) {
  const s = useStore.getState()
  if (s.recording) s.set({ recording: { ...s.recording, bytes: s.recording.bytes + n } })
}

/** Opus packet received for the selected service. */
export function recordOpus(sid: number, data: Uint8Array) {
  if (active?.format === 'opus' && active.sid === sid && active.ogg) {
    const before = active.ogg.bytes
    active.ogg.packet(data)
    bump(active.ogg.bytes - before)
  }
}

/** Original encoded audio (LOAS AAC or MP2) sent by the server. */
export function recordEncoded(sid: number, aac: boolean, data: Uint8Array) {
  if (active?.format === 'aac' && active.sid === sid) {
    active.aac = aac
    active.raw.push(data.slice())
    bump(data.length)
  }
}
