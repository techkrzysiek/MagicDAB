import type { Ensemble, Service } from './types'
import { saveInApp } from './desktop'

export function mhz(khz: number) {
  return (khz / 1000).toFixed(3)
}

export function fmMhz(khz: number) {
  return (khz / 1000).toFixed(1)
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
