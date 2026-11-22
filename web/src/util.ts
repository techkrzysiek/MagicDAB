import type { AudioFormat, Component, Ensemble, Service } from './types'
import { saveInApp } from './desktop'

export function mhz(khz: number) {
  return (khz / 1000).toFixed(3)
}

/** Power in kW as the database has it: 7.94, not 7.940000057220459 (a 32-bit float). */
export function kw(v: number) {
  return String(Math.round(v * 1000) / 1000)
}

export function fmMhz(khz: number) {
  return (khz / 1000).toFixed(1)
}

/** How the audio of a station is coded: "AAC 24 kHz + SBR → 48 kHz + PS". */
export function audioCoding(f: AudioFormat) {
  const khz = (hz: number) => `${hz / 1000} kHz`
  // (24 kHz and less: the half rates of MPEG-2)
  if (f.codec === 'MP2') return `MPEG-${f.sampleRate < 32000 ? 2 : 1} Audio Layer II ${khz(f.sampleRate)}`
  if (!f.sbr) return `AAC-LC ${khz(f.sampleRate)}`
  return `AAC ${khz(f.sampleRate / 2)} + SBR → ${khz(f.sampleRate)}${f.ps ? ' + PS' : ''}`
}

export function hex(v: number | null | undefined, digits = 4) {
  if (v === null || v === undefined) return ''
  return v.toString(16).toUpperCase().padStart(digits, '0')
}

/** ECC-qualified service id, e.g. E2A5C1. */
export function fullSid(s: Service) {
  if (s.isData && s.sid > 0xffff) return hex(s.sid, 8)
  return (s.ecc ? hex(s.ecc, 2) : '') + hex(s.sid)
}

export function channelForKhz(channels: { name: string; frequency: number }[] | undefined, khz: number) {
  return channels?.find((c) => c.frequency === khz)?.name ?? ''
}

/** Initials used for a service without logo. */
export function initials(label: string) {
  const words = label.replace(/[^\p{L}\p{N} ]/gu, ' ').split(/\s+/).filter(Boolean)
  if (words.length === 0) return '?'
  if (words.length === 1) return words[0].slice(0, 3).toUpperCase()
  return (words[0][0] + words[1][0] + (words[2]?.[0] ?? '')).toUpperCase()
}

/** Stable pastel colour derived from a service id. */
export function serviceColor(sid: number) {
  const h = (sid * 47) % 360
  return `hsl(${h} 55% 45%)`
}

/** SNR thresholds, shifted by the protection of the audio service. */
const SNR_OFFSETS: Record<string, number> = {
  'UEP 1': 1.0, 'UEP 2': 2.0, 'UEP 3': 3.0, 'UEP 4': 4.0, 'UEP 5': 5.5,
  'EEP 1-A': -0.8, 'EEP 2-A': 0.0, 'EEP 3-A': 1.2, 'EEP 4-A': 4.0,
  'EEP 1-B': 0.4, 'EEP 2-B': 1.9, 'EEP 3-B': 2.7, 'EEP 4-B': 4.6,
}

export function signalLevel(synced: boolean, signal: boolean, snr: number, protection?: string | null): 0 | 1 | 2 | 3 {
  if (!signal && !synced) return 0
  const off = protection ? (SNR_OFFSETS[protection] ?? 0) : 0
  if (!synced) return 1
  if (snr < 7 + off) return 1
  if (snr < 10 + off) return 2
  return 3
}

export function findService(ens: Ensemble, sid: number | undefined | null) {
  if (sid === undefined || sid === null) return undefined
  return ens.services?.find((s) => s.sid === sid)
}

/** The announcement types of FIG 0/18 and 0/19 as the server names them (asw bits 0..10). */
export const ANNOUNCEMENT_TYPES = [
  'Alarm',
  'Road Traffic flash',
  'Transport flash',
  'Warning/Service',
  'News flash',
  'Area weather flash',
  'Event announcement',
  'Special event',
  'Programme Information',
  'Sport report',
  'Financial report',
]

export type Announcement = NonNullable<Ensemble['announcements']>[number]

/** Announcements on air now that concern `svc`: its cluster (or the alarm
 *  cluster 0xFF of all services) and a type it supports (FIG 0/18). */
export function announcementsFor(ens: Ensemble, svc: Service | undefined): Announcement[] {
  if (!svc) return []
  return (ens.announcements ?? []).filter(
    (a) => (a.cluster === 0xff || svc.clusters.includes(a.cluster)) && a.types.some((t) => t === 'Alarm' || svc.asu.includes(t)),
  )
}

/** The service whose audio is the announcement's sub-channel (its primary component). */
export function announcementSource(ens: Ensemble, a: Announcement): Service | undefined {
  return ens.services?.find((s) => s.components.some((c) => c.primary && c.tmid === 0 && c.subch === a.subch))
}

/** Audio components of a service besides its primary one (SCIdS 1, 2...):
 *  programmes of their own, on their own sub-channel, under its SId. */
export function secondaries(svc: Service): Component[] {
  return svc.components.filter((c) => !c.primary && c.tmid === 0 && !!c.scids)
}

/** Name of a secondary component: its label (FIG 1/4), else the service's with its number. */
export function componentLabel(svc: Service, c: Component) {
  return c.label?.trim() || `${svc.label} ${(c.scids ?? 0) + 1}`
}

/** Key of a component's "now playing" line (dlsAll): the SId, "SId:SCIdS" for a secondary one. */
export function dlsKey(sid: number, scids?: number | null) {
  return scids ? `${sid}:${scids}` : String(sid)
}

const asComponent = new WeakMap<Service, Map<number, Service>>()

/** What plays: the service, or for a secondary component the service as that
 *  component (its label, its sub-channel, `scids`). The same object as long as
 *  the ensemble's is, so a store selector may return it. */
export function playingService(ens: Ensemble, sel: { sid: number; scids: number } | null | undefined): Service | undefined {
  const svc = findService(ens, sel?.sid)
  if (!svc || !sel?.scids) return svc
  const c = secondaries(svc).find((x) => x.scids === sel.scids)
  if (!c) return svc
  let byScids = asComponent.get(svc)
  if (!byScids) asComponent.set(svc, (byScids = new Map()))
  let v = byScids.get(sel.scids)
  if (!v) {
    const sc = ens.subchannels?.find((x) => x.id === c.subch)
    const label = componentLabel(svc, c)
    v = {
      ...svc,
      scids: sel.scids,
      label,
      short: c.short?.trim() || label,
      long: c.long ?? null,
      language: c.language ?? svc.language,
      ca: c.ca,
      codec: c.ctype === 63 ? 'DAB+' : 'DAB',
      ascty: c.ctype,
      bitrate: c.bitrate ?? sc?.bitrate ?? null,
      subchannel: c.subch,
      protection: c.protection ?? sc?.protection ?? null,
      codeRate: sc?.codeRate ?? null,
      startCu: sc?.start ?? null,
      sizeCu: sc?.size ?? null,
    }
    byScids.set(sel.scids, v)
  }
  return v
}

export function formatDuration(sec: number) {
  const m = Math.floor(sec / 60)
  const s = Math.floor(sec % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

export function ago(unix: number) {
  const d = Date.now() / 1000 - unix
  if (d < 90) return `${Math.round(d)} s`
  if (d < 5400) return `${Math.round(d / 60)} min`
  if (d < 172800) return `${Math.round(d / 3600)} h`
  return `${Math.round(d / 86400)} d`
}

export function downloadText(name: string, text: string, mime = 'text/csv') {
  if (saveInApp(name, new Blob([text], { type: mime }))) return
  const url = URL.createObjectURL(new Blob([text], { type: mime }))
  const a = document.createElement('a')
  a.href = url
  a.download = name
  a.click()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

export function downloadBlob(name: string, blob: Blob) {
  if (saveInApp(name, blob)) return
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  document.body.appendChild(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 5000)
}

/** Short age of a unix timestamp, e.g. "5 min", "3 h", "2 d". */
export function ageShort(ts: number, lang: string) {
  const s = Math.max(0, Date.now() / 1000 - ts)
  if (s < 90) return lang === 'pl' ? 'teraz' : 'now'
  if (s < 3600) return `${Math.round(s / 60)} min`
  if (s < 86400 * 2) return `${Math.round(s / 3600)} h`
  return `${Math.round(s / 86400)} d`
}

/** Time of a unix timestamp, with the date when it is not today. */
export function whenShort(ts: number) {
  const d = new Date(ts * 1000)
  const pad = (n: number) => n.toString().padStart(2, '0')
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`
  const today = new Date()
  if (d.toDateString() === today.toDateString()) return time
  return `${pad(d.getDate())}.${pad(d.getMonth() + 1)} ${time}`
}
