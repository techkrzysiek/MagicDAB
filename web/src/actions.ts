import { useStore, save, sessionSet, fixedChannelOf } from './store'
import { dropSlide, send } from './ws'
import { player } from './audio/player'
import { initMediaSession, mediaStart, mediaStop } from './audio/mediasession'

/** Counts starts and stops: a stop while a start is still loading wins. */
let audioRequest = 0

/** Start audio playback; must be called from a user gesture. */
export async function startAudio() {
  const s = useStore.getState()
  const request = ++audioRequest
  // the media element must start within the user gesture (iOS)
  mediaStart()
  try {
    await player.start()
  } catch (e) {
    console.error(e)
    mediaStop()
    s.notify('error', 'Audio: ' + String(e))
    return
  }
  if (request !== audioRequest) {
    // stopped meanwhile (e.g. the media notification paused for a call)
    player.suspend()
    return
  }
  useStore.getState().set({ audioEnabled: true })
  send({ type: 'audio', enabled: true, codec: useStore.getState().codec })
}

export function stopAudio() {
  audioRequest++
  useStore.getState().set({ audioEnabled: false, audioPlaying: false })
  send({ type: 'audio', enabled: false })
  player.suspend()
  mediaStop()
}

/** Previous / next station of the current multiplex (media keys, notification). */
export function stepService(dir: 1 | -1) {
  const s = useStore.getState()
  const list = (s.ensemble.services ?? []).filter((x) => x.isAudio)
  // (FM: no stations of a multiplex to step through)
  if (!list.length || s.selected?.sid === FM_SID) return
  const i = list.findIndex((x) => x.sid === s.selected?.sid)
  const next = list[(i + dir + list.length) % list.length]
  selectService(next.sid, 0)
}

initMediaSession({ play: () => startAudio(), stop: () => stopAudio(), step: stepService })

export function toggleAudio() {
  if (useStore.getState().audioEnabled) stopAudio()
  else startAudio()
}

/** The tuner is locked for this client: no admin login, no tuning password. */
export function tuningLocked(s = useStore.getState()) {
  return !!s.hello?.tunerLocked && !s.admin && !s.tuner
}

/** The channel may be tuned by this client (listeners: Settings > Access). */
export function channelAllowed(channel: string, s = useStore.getState()) {
  const allowed = s.hello?.allowedChannels ?? []
  return s.admin || !allowed.length || allowed.includes(channel.toUpperCase())
}

/** The channel plays on a receiver of this listener's own, the main one stays
 *  (a mapped channel while the server allows it; the admin moves the main one). */
export function ownMultiplex(channel: string, s = useStore.getState()) {
  const ch = channel.toUpperCase()
  return !!s.hello?.listenerMultiplex && !s.admin && (s.state?.mappedChannels ?? []).some((c) => c.toUpperCase() === ch)
}

/** Says why this client may not tune (to `channel`): true when it may not. */
function tuneRefused(s: ReturnType<typeof useStore.getState>, channel?: string) {
  // (the tuner lock is for the main receiver)
  if (tuningLocked(s) && !(channel && ownMultiplex(channel, s))) {
    s.notify('warning', 'tuner_locked')
    return true
  }
  if (channel && !channelAllowed(channel, s)) {
    s.notify('warning', 'channel_not_allowed')
    return true
  }
  return false
}

/** Select a service; `channel` is given when the service is on another ensemble. */
export function selectService(sid: number, scids = 0, channel?: string) {
  const s = useStore.getState()
  const cur = s.ensemble.channel
  if (channel && channel !== cur && tuneRefused(s, channel)) return
  if (s.selected?.sid !== sid || (channel && channel !== cur)) dropSlide(sid)
  s.set({ selected: { sid, scids } })
  save('selected', { sid, scids })
  player.flush()
  send({ type: 'select', sid, scids, channel: channel ?? cur ?? undefined })
  if (!s.audioEnabled) startAudio()
}

/** Service id of the FM tuner's audio. */
export const FM_SID = 0xffffffff

export function fmTune(khz: number) {
  if (tuneRefused(useStore.getState())) return
  send({ type: 'fmTune', khz: Math.round(khz) })
}

export function linkingCommand(p: { mode?: 'auto' | 'dab' | 'fm'; ab?: boolean; search?: boolean; enabled?: boolean }) {
  if (tuneRefused(useStore.getState())) return
  send({ type: 'linking', ...p })
}

export function fmAntenna(n: number) {
  if (tuneRefused(useStore.getState())) return
  send({ type: 'fmAntenna', antenna: n })
}

export function fmScan() {
  send({ type: 'fmScan' })
}

export function tune(channel: string) {
  if (tuneRefused(useStore.getState(), channel)) return
  send({ type: 'tune', channel })
}

export function stepChannel(dir: 1 | -1) {
  const s = useStore.getState()
  if (fixedChannelOf(s)) {
    s.notify('warning', 'channel_fixed')
    return
  }
  const all = (s.hello?.channels ?? []).filter((c) => channelAllowed(c.name, s))
  // without a tuner, or with the tuner locked and own multiplexes: the mapped channels
  const mappedOnly = s.state?.noTuner || (tuningLocked(s) && !!s.hello?.listenerMultiplex)
  const chans = mappedOnly ? all.filter((c) => (s.state?.mappedChannels ?? []).includes(c.name)) : all
  const i = chans.findIndex((c) => c.name === s.state?.channel)
  const n = chans[(i + dir + chans.length) % chans.length]
  if (n) tune(n.name)
}

export function setVolume(v: number) {
  useStore.getState().set({ volume: v, muted: false })
  save('volume', v)
  player.applyVolume()
}

/** Louder or quieter by `delta` (keyboard); unmutes like the slider. */
export function stepVolume(delta: number) {
  const v = useStore.getState().volume
  setVolume(Math.round(Math.min(1, Math.max(0, v + delta)) * 100) / 100)
}

export function toggleMute() {
  const s = useStore.getState()
  s.set({ muted: !s.muted })
  player.applyVolume()
}

export function setCodec(codec: 'opus' | 'pcm') {
  const s = useStore.getState()
  s.set({ codec })
  save('codec', codec)
  if (s.audioEnabled) send({ type: 'audio', enabled: true, codec })
}

export function login(password: string) {
  sessionSet('magicdab.pw', password)
  send({ type: 'login', password })
}

export function logout() {
  sessionSet('magicdab.pw', null)
  send({ type: 'logout' })
}

/** The gate of a private server: the administrator or the tuning password
 *  (the server tells which it was; ws.ts keeps it for reconnects). */
export function enter(password: string) {
  useStore.getState().set({ gate: { pending: password, error: false, locked: false } })
  send({ type: 'enter', password })
}

/** The answer to "are you still listening?". */
export function stillHere() {
  useStore.getState().set({ stillHere: null })
  send({ type: 'stillHere' })
}

/** Unlock tuning with the tuning password (kept for reconnects as the admin's). */
export function tuneLogin(password: string) {
  sessionSet('magicdab.tunepw', password)
  send({ type: 'tuneLogin', password })
}

export function tuneLogout() {
  sessionSet('magicdab.tunepw', null)
  send({ type: 'tuneLogout' })
}

/** Admin: close the connections of an address, or ban it (`hours` 0: until lifted). */
export function kickAddress(address: string) {
  send({ type: 'kick', address })
}

export function banAddress(address: string, reason: string, hours = 0) {
  send({ type: 'ban', address, reason, hours })
}

export function unbanAddress(address: string) {
  send({ type: 'unban', address })
}

export function startScan() {
  if (tuneRefused(useStore.getState())) return
  send({ type: 'scan', action: 'start' })
}

export function stopScan() {
  send({ type: 'scan', action: 'stop' })
}

export function saveSettings(settings: Record<string, unknown>) {
  send({ type: 'settings', settings })
}

/** Import a transmitter list; a big one goes in parts, the server takes small messages only. */
export function importTxDb(csv: string) {
  const size = 256 * 1024
  if (csv.length <= size) return send({ type: 'txdb', csv })
  for (let start = 0, part = 0; start < csv.length; part++) {
    let end = Math.min(csv.length, start + size)
    // a character outside the BMP must not be split between two parts
    const c = csv.charCodeAt(end - 1)
    if (end < csv.length && c >= 0xd800 && c <= 0xdbff) end--
    send({ type: 'txdbPart', part, last: end >= csv.length, csv: csv.slice(start, end) })
    start = end
  }
}
