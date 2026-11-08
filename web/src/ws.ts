// WebSocket connection to the MagicDAB server.
import { useStore, save, sessionGet, sessionSet } from './store'
import type { LogoVariant, Stats, StatsPoint } from './types'
import { desktopToken } from './desktop'
import { restartChat } from './chat'
import { player } from './audio/player'
import { recordEncoded, recordingActive } from './audio/recorder'

let socket: WebSocket | null = null
let retry = 0
let retryTimer = 0
const textDecoder = new TextDecoder()

export function send(msg: Record<string, unknown>) {
  if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg))
}

function wsUrl() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const base = location.pathname.replace(/[^/]*$/, '')
  return `${proto}//${location.host}${base}ws`
}

/**
 * Reload when the server has a newer UI build than the one running (after a
 * deploy, or when a browser served an outdated index.html from its cache).
 */
async function checkUiVersion() {
  const mine = document.querySelector<HTMLScriptElement>('script[type="module"][src]')?.getAttribute('src')
  if (!mine) return
  try {
    const html = await (await fetch(location.pathname.replace(/[^/]*$/, '') + 'index.html', { cache: 'no-store' })).text()
    const current = html.match(/<script[^>]+type="module"[^>]+src="([^"]+)"/)?.[1]
    if (!current || current === mine) return
    const key = 'magicdab-reloaded-for'
    let done: string | null = null
    try {
      done = sessionStorage.getItem(key)
      sessionStorage.setItem(key, current)
    } catch {
      /* storage unavailable */
    }
    if (done === current) return
    // reloading stops the audio (browsers need a tap to start it again)
    if (useStore.getState().audioEnabled) useStore.getState().set({ uiUpdate: true })
    else location.reload()
  } catch {
    /* offline or proxy without index.html: keep running */
  }
}

// a new interface can come without a restart of the server, so without a new
// connection: look every 5 minutes, and when the page is shown again
setInterval(checkUiVersion, 5 * 60_000)
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') checkUiVersion()
})

function handleText(m: any) {
  const st = useStore.getState()
  switch (m.type) {
    case 'hello':
      // at the gate of a private server (not let in, or not any more): nothing of the receiver is shown
      if (m.private && !m.admin && !m.tuner) toGate()
      st.set({ hello: m, admin: m.admin, tuner: !!m.tuner })
      checkUiVersion()
      break
    case 'enter': {
      // the gate of a private server: the accepted password is kept for reconnects as the admin's
      const pw = st.gate.pending
      if (m.ok && pw) sessionSet(m.admin ? 'magicdab.pw' : 'magicdab.tunepw', pw)
      st.set({
        admin: !!m.admin,
        tuner: !!m.tuner,
        chatToken: m.chatToken ?? st.chatToken,
        gate: { pending: null, error: !m.ok && !m.locked, locked: !!m.locked },
      })
      break
    }
    case 'stillHere':
      st.set({ stillHere: m.ask === false ? null : { seconds: Number(m.seconds) || 60, at: Date.now() } })
      break
    case 'tuneLogin':
      // (on a private server a trusted listener gets a token for the chat)
      st.set({ tuner: !!m.tuner, ...(m.ok && m.chatToken ? { chatToken: m.chatToken } : {}) })
      if (!m.ok) {
        // a stored password that no longer works
        sessionSet('magicdab.tunepw', null)
        // (too many wrong ones: the server says so in a notice)
        if (st.tuneOpen) st.set({ tuneError: true })
        else if (!m.locked) st.notify('error', 'tune_failed')
      }
      break
    case 'listenerList':
      st.set({ listenerList: { clients: m.clients ?? [], self: m.self } })
      break
    case 'state':
      st.set({ state: m, listeners: m.listeners })
      break
    case 'spiDocs': {
      // drop cached XML of documents that were replaced
      const known = new Map(st.spiDocs.map((d) => [d.id, d.received]))
      const xml = { ...st.spiDocXml }
      for (const d of m.docs) if (known.get(d.id) !== d.received) delete xml[d.id]
      st.set({ spiDocs: m.docs, spiDocXml: xml })
      break
    }
    case 'update':
      st.set({ update: m })
      break
    case 'gallery': {
      // pictures of slides no longer kept
      const keep = new Set<number>(m.slides.map((x: { id: number }) => x.id))
      const urls: Record<number, string> = {}
      for (const [id, url] of Object.entries(st.galleryUrls)) {
        if (keep.has(+id)) urls[+id] = url
        else URL.revokeObjectURL(url)
      }
      st.set({ gallery: { sid: m.sid, slides: m.slides }, galleryUrls: urls })
      break
    }
    case 'tunnelStatus':
      st.set({ tunnelStatus: m.status })
      break
    case 'serverMap':
      st.set({ serverMapStatus: m.status })
      break
    case 'dataProgress':
      st.set({ dataProgress: m.items })
      break
    case 'spiDoc':
      st.set({ spiDocXml: { ...st.spiDocXml, [m.id]: m.xml } })
      break
    case 'signal': {
      const hist = st.snrHistory
      const now = Date.now() / 1000
      const next = m.synced ? [...hist, { t: now, snr: m.snr }].filter((p) => now - p.t < 600) : hist
      st.set({ signal: m, snrHistory: next })
      break
    }
    case 'ensemble':
      if (m.channel !== st.ensemble.channel) st.set({ dlsAll: {} })
      // slides and texts belong to a multiplex: another one may not carry them
      if (m.channel !== st.ensemble.channel || (m.eid != null && st.ensemble.eid != null && m.eid !== st.ensemble.eid)) {
        for (const d of Object.values(st.services)) {
          for (const old of [d?.slide?.url, d?.visSlide?.url]) if (old) setTimeout(() => URL.revokeObjectURL(old), 1000)
        }
        st.set({ services: {} })
      }
      st.set({ ensemble: m })
      break
    case 'serviceList':
      st.set({ serviceList: m.ensembles })
      break
    case 'time':
      st.set({ time: { utc: m.utc, lto: m.lto ?? 0, at: Date.now() } })
      break
    case 'tii':
      st.set({ tii: m })
      break
    case 'tiiPairs':
      st.set({ tiiPairs: m.pairs })
      break
    case 'listeners':
      st.set({ listeners: m.count })
      break
    case 'dxResults':
      st.set({ dxResults: m.results ?? [] })
      break
    case 'rawRecord':
      st.set({ rawRecord: m })
      break
    case 'dxrec': {
      // (progress messages come without the list: it stays)
      const { type: _t, ...rest } = m
      const keep = st.dxrec && rest.recordings === undefined ? { recordings: st.dxrec.recordings, usedMb: st.dxrec.usedMb } : {}
      st.set({ dxrec: { ...st.dxrec, ...keep, ...rest } })
      break
    }
    case 'logos': {
      const logos: Record<string, LogoVariant[]> = m.logos ?? {}
      // pictures of logos no longer listed, or received anew (a station's new
      // logo keeps the place of the old one), are released
      const version = (l: Record<string, LogoVariant[]>) =>
        new Map(Object.values(l).flatMap((vs) => vs.map((v) => [v.id, `${v.received}:${v.size}`] as const)))
      const listed = version(logos)
      const before = version(st.logos)
      const logoUrls = { ...st.logoUrls }
      for (const [id, url] of Object.entries(logoUrls)) {
        if (!listed.has(id) || listed.get(id) !== before.get(id)) {
          URL.revokeObjectURL(url)
          delete logoUrls[id]
          requested.delete(id)
        }
      }
      st.set({ logos: m.logos ?? {}, texts: m.texts ?? {}, logoUrls })
      break
    }
    case 'select': {
      const cur = st.selected
      // the server's choice for new clients: after a reconnect (e.g. a server
      // restart) the own one stays, unless the multiplex does not have it
      const known = !st.ensemble.valid || st.ensemble.services?.some((x) => x.sid === cur?.sid)
      if (m.reason === 'initial' && cur && known) break
      if (!cur || cur.sid !== m.sid || cur.scids !== m.scids) {
        dropSlide(m.sid)
        st.set({ selected: { sid: m.sid, scids: m.scids } })
        save('selected', { sid: m.sid, scids: m.scids })
        // (auto: the server's choice, no action of the listener)
        send({ type: 'select', sid: m.sid, scids: m.scids, auto: true })
      }
      break
    }
    case 'dls':
      st.setService(m.sid, { dls: { text: m.text, tags: m.tags, itemToggle: m.itemToggle, itemRunning: m.itemRunning } })
      st.set({ dlsAll: { ...st.dlsAll, [m.sid]: m.text } })
      break
    case 'epgIndex':
      st.set({ epgIndex: m.services ?? {} })
      break
    case 'epg':
      st.set({ epg: { ...st.epg, [m.sid]: m.programmes ?? [] } })
      break
    case 'changeLog':
      st.set({ changeLog: m.events ?? [] })
      break
    case 'change':
      st.set({ changeLog: [m.event, ...st.changeLog].slice(0, 500) })
      break
    case 'changeReport': {
      // (the report of a new multiplex onto its event)
      const e = m.event
      st.set({ changeLog: st.changeLog.map((x) => (x.time === e.time && x.channel === e.channel && x.eid === e.eid ? e : x)) })
      break
    }
    case 'dlsAll':
      st.set({ dlsAll: { ...st.dlsAll, [m.sid]: m.text } })
      break
    case 'dlsAllBulk': {
      const lines: Record<number, string> = {}
      for (const [k, v] of Object.entries(m.lines ?? {})) lines[Number(k)] = v as string
      st.set({ dlsAll: lines })
      break
    }
    case 'audioFormat':
      st.setService(m.sid, { format: m })
      break
    case 'stats':
      st.setService(m.sid, { stats: m })
      break
    case 'fm':
      st.set({ fm: m })
      break
    case 'fmScan':
      st.set({ fmScan: m.points ?? [] })
      break
    case 'linking':
      st.set({ linking: m })
      break
    case 'linkInfo':
      st.set({ linkInfo: m })
      break
    case 'visText':
      st.setService(m.sid, { visText: m.text })
      break
    case 'slideProgress': {
      const p = m.progress
      const old = st.services[m.sid]?.slideProgress
      st.setService(m.sid, {
        slideProgress: p?.size
          ? { bytes: p.bytes, size: p.size, name: p.contentName, at: Date.now() }
          : old && { ...old, bytes: old.size, done: true },
      })
      break
    }
    case 'login':
      st.set({ admin: !!m.admin, chatToken: m.chatToken ?? null })
      if (!m.ok) {
        // a stored password that no longer works
        sessionSet('magicdab.pw', null)
        if (st.loginOpen) st.set({ loginError: true })
        else st.notify('error', 'login_failed')
      }
      break
    case 'settings':
      st.set({ settings: m })
      break
    case 'notice':
      st.notify(m.level, m.message)
      break
    case 'pong': {
      const t = Number(m.t)
      if (!isFinite(t)) break
      if (t === pingOut) pingOut = null
      waitingSince = null
      st.set({ latency: { ms: Math.max(0, Math.round(performance.now() - t)), late: false } })
      break
    }
    case 'serverStats': {
      const { history, ...stats } = m as Stats & { history?: StatsPoint[] }
      let points = history ?? st.statsHistory
      if (!history && stats.system) {
        points = [...points, [Math.round(Date.now() / 1000), stats.system.cpu, stats.process.cpu, stats.net.out] as StatsPoint]
      }
      // (the last hour)
      const since = Date.now() / 1000 - 3600
      st.set({ stats, statsHistory: points.filter((p) => p[0] >= since) })
      break
    }
    case 'logHistory':
      st.set({ logLines: m.lines })
      break
    case 'log':
      st.set({ logLines: [...st.logLines.slice(-999), m.line] })
      break
  }
}

/**
 * To the gate of a private server: what the page knows of the receiver goes
 * (the server sends nothing more until a password is given; the gate
 * component stops the sound).
 */
function toGate() {
  const st = useStore.getState()
  for (const d of Object.values(st.services)) {
    for (const old of [d?.slide?.url, d?.visSlide?.url]) if (old) setTimeout(() => URL.revokeObjectURL(old), 1000)
  }
  st.set({
    state: null,
    signal: null,
    ensemble: { valid: false, channel: null, frequency: 0 },
    services: {},
    serviceList: [],
    dlsAll: {},
    tii: null,
    settings: null,
    listenerList: null,
    update: null,
    stats: null,
    chatToken: null,
    chatOpen: false,
    stillHere: null,
  })
}

/** Forget the slideshow state kept from an earlier visit of a service: only
 *  the slideshow of the selected service reaches the client, so it may be
 *  outdated. The server answers the selection with the current one. */
export function dropSlide(sid: number) {
  const st = useStore.getState()
  const d = st.services[sid]
  st.setService(sid, { slide: undefined, slideProgress: undefined, visSlide: undefined, visText: undefined })
  for (const old of [d?.slide?.url, d?.visSlide?.url]) if (old) setTimeout(() => URL.revokeObjectURL(old), 1000)
}

function handleBinary(buf: ArrayBuffer) {
  const b = new Uint8Array(buf)
  const kind = b[0]
  if (kind === 1) {
    player.packet(b)
    return
  }
  const hlen = new DataView(buf).getUint32(1, true)
  const header = JSON.parse(textDecoder.decode(b.subarray(5, 5 + hlen)))
  const payload = b.subarray(5 + hlen)
  const st = useStore.getState()
  if (kind === 2 && header.kind === 'gallery') {
    const url = URL.createObjectURL(new Blob([payload], { type: header.mime }))
    st.set({ galleryUrls: { ...st.galleryUrls, [header.id]: url } })
    return
  }
  if (kind === 2) {
    const blob = new Blob([payload], { type: header.mime })
    const url = URL.createObjectURL(blob)
    const vis = header.source === 'RadioVIS'
    const old = vis ? st.services[header.sid]?.visSlide?.url : st.services[header.sid]?.slide?.url
    const slide = {
      url,
      contentName: header.contentName,
      mime: header.mime,
      size: header.size,
      categoryTitle: header.categoryTitle ?? null,
      clickThroughUrl: header.clickThroughUrl ?? null,
      received: header.received,
    }
    st.setService(header.sid, vis ? { visSlide: slide } : { slide })
    if (old) setTimeout(() => URL.revokeObjectURL(old), 1000)
  } else if (kind === 5) {
    recordEncoded(header.sid, !!header.aac, payload)
  } else if (kind === 4) {
    const url = URL.createObjectURL(new Blob([payload], { type: header.mime }))
    // (the same logo again after a reconnect)
    const old = st.logoUrls[header.id]
    st.set({ logoUrls: { ...st.logoUrls, [header.id]: url } })
    if (old) setTimeout(() => URL.revokeObjectURL(old), 1000)
  } else if (kind === 3) {
    const n = header.bins as number
    const f = new Float32Array(payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength))
    // then the constellation (re, im) and the impulse response (newer servers)
    const pts = 2 * ((header.points as number | undefined) ?? 0)
    const taps = (header.taps as number | undefined) ?? 0
    st.set({
      spectrum: {
        frequency: header.frequency,
        offset: header.offset,
        spectrum: f.subarray(0, n),
        nullSpectrum: f.subarray(n, 2 * n),
        hasNull: header.null !== false,
        constellation: f.subarray(2 * n, 2 * n + pts),
        impulse: f.subarray(2 * n + pts, 2 * n + pts + taps),
        advance: header.advance,
        guard: header.guard,
        noise: header.noise,
        received: Date.now(),
      },
    })
  }
}

const requested = new Set<string>()
/** Request a logo image once per session. */
export function requestLogo(id: string) {
  if (requested.has(id)) return
  requested.add(id)
  send({ type: 'getLogo', id })
}

export function connect() {
  const st = useStore.getState()
  st.set({ connection: 'connecting' })
  const ws = new WebSocket(wsUrl())
  ws.binaryType = 'arraybuffer'
  socket = ws
  ws.onopen = () => {
    retry = 0
    requested.clear()
    useStore.getState().set({ connection: 'open' })
    const s = useStore.getState()
    // restore session state after a reconnect
    const pw = sessionGet('magicdab.pw')
    // the program's own window logs in with its token
    if (desktopToken) send({ type: 'login', desktop: desktopToken, auto: true })
    else if (pw) send({ type: 'login', password: pw, auto: true })
    const tunePw = sessionGet('magicdab.tunepw')
    if (tunePw) send({ type: 'tuneLogin', password: tunePw, auto: true })
    // a listener on a multiplex of its own goes back to it (if the server still lets it)
    // (auto: restored, not an action of the listener)
    const own = s.state?.slot && !s.admin ? s.state.channel : null
    if (s.selected) send({ type: 'select', sid: s.selected.sid, scids: s.selected.scids, auto: true, ...(own ? { channel: own, rejoin: true } : {}) })
    else if (own) send({ type: 'tune', channel: own, rejoin: true, auto: true })
    if (s.audioEnabled) send({ type: 'audio', enabled: true, codec: s.codec, auto: true })
    // the server keeps the recording flag per connection
    if (recordingActive()?.format === 'aac') send({ type: 'record', enabled: true, auto: true })
    resubscribe()
    // why the previous connection ended, for the server log
    if (lastClose) {
      send({
        type: 'diag',
        event: 'reconnected',
        code: lastClose.code,
        reason: lastClose.reason,
        clean: lastClose.clean,
        downMs: Date.now() - lastClose.at,
        online: navigator.onLine,
        visible: document.visibilityState === 'visible',
      })
      lastClose = null
    }
    // (after the burst of the messages above)
    pingOut = waitingSince = null
    setTimeout(pingTick, 1000)
  }
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') {
      try {
        handleText(JSON.parse(ev.data))
      } catch (e) {
        console.error('bad message', e)
      }
    } else {
      handleBinary(ev.data as ArrayBuffer)
    }
  }
  ws.onclose = (ev) => {
    lastClose ??= { code: ev.code, reason: ev.reason, clean: ev.wasClean, at: Date.now() }
    useStore.getState().set({ connection: 'closed', latency: null })
    socket = null
    // kicked by an admin, no answer to "still listening?", or refused (banned): no reconnecting by itself
    if (ev.code === 4001 || ev.code === 4002 || ev.code === 4003) {
      useStore.getState().set({ closedBy: ev.code === 4001 ? 'kicked' : ev.code === 4002 ? 'idle' : 'refused', stillHere: null })
      return
    }
    retry = Math.min(retry + 1, 10)
    retryTimer = window.setTimeout(connect, 500 * retry)
  }
}

/** Connect again after a kick or an unanswered "still listening?" (the button of the banner). */
export function reconnectAfterKick() {
  useStore.getState().set({ closedBy: null })
  lastClose = null
  retry = 0
  connect()
  restartChat()
}

/** Back on the page (phone unlocked) or the network back: connect now instead of
 *  waiting for a timer, which browsers slow down to once a minute in the background. */
function reconnectNow() {
  if (socket || useStore.getState().connection !== 'closed' || useStore.getState().closedBy) return
  clearTimeout(retryTimer)
  retry = 0
  connect()
}
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') reconnectNow()
})
window.addEventListener('online', reconnectNow)

let lastClose: { code: number; reason: string; clean: boolean; at: number } | null = null

/**
 * Latency for the status bar: a ping over this connection every few seconds.
 * The server answers it in its radio thread, so a busy server counts too.
 */
const PING_EVERY = 5000
/** performance.now() (whole ms: echoed exactly) of the unanswered ping, and of
 *  the first one since the last answer */
let pingOut: number | null = null
let waitingSince: number | null = null
function pingTick() {
  if (!socket || socket.readyState !== WebSocket.OPEN) return
  const now = Math.round(performance.now())
  if (pingOut !== null) {
    // still unanswered: shown as late, for as long as no answer came
    const waited = now - (waitingSince ?? pingOut)
    if (waited > PING_EVERY) useStore.getState().set({ latency: { ms: waited, late: true } })
    // (after a while a new one: the server may have dropped the answer)
    if (now - pingOut < 15000) return
  }
  pingOut = now
  waitingSince ??= now
  send({ type: 'ping', t: now })
}
setInterval(pingTick, PING_EVERY)

/** Errors of the interface go to the server log (with the stack). */
export function reportUiError(message: string, stack?: string) {
  send({ type: 'diag', event: 'uiError', message: String(message).slice(0, 500), stack: (stack ?? '').slice(0, 4000), view: useStore.getState().view })
}
window.addEventListener('error', (e) => reportUiError(e.message, e.error?.stack))
window.addEventListener('unhandledrejection', (e) => reportUiError(String(e.reason?.message ?? e.reason), e.reason?.stack))

// audio dropouts of the player, reported to the server log every 15 s
let reportedUnderruns = 0
setInterval(() => {
  const s = useStore.getState()
  const n = s.audioUnderruns ?? 0
  if (n < reportedUnderruns) reportedUnderruns = 0
  if (n > reportedUnderruns && s.audioEnabled) {
    send({
      type: 'diag',
      event: 'underruns',
      count: n - reportedUnderruns,
      buffer: Math.round((s.audioBuffer ?? 0) * 1000),
      target: Math.round((s.audioTarget ?? 0) * 1000),
      visible: document.visibilityState === 'visible',
    })
  }
  reportedUnderruns = n
}, 15000)

// current subscriptions derived from the visible view
let subs = { spectrum: false, tiiPairs: false, log: false, linking: false, serverStats: false }
export function setSubscriptions(p: Partial<typeof subs>) {
  subs = { ...subs, ...p }
  resubscribe()
}
function resubscribe() {
  send({ type: 'subscribe', ...subs })
}
