import { useCallback } from 'react'
import { create } from 'zustand'
import type {
  AudioFormat, AudioStats, ChangeEvent, DxResult, Dls, Ensemble, EnsembleEntry, Hello, LogLine, LogoVariant, RadioState, ServiceText, Signal,
  Slide, SpiDoc, TiiState, DataProgress, FmState, LinkingState, LinkInfo, Listener, Stats, StatsPoint, DxRecState,
} from './types'
import { androidApp } from './desktop'

export interface GallerySlide {
  id: number
  /** category and slide id of a categorised slideshow (CatSLS) */
  category: number | null
  slide: number | null
  categoryTitle: string | null
  clickThroughUrl: string | null
  contentName: string
  mime: string
  received: number
}

export type View = 'service' | 'stations' | 'linking' | 'info' | 'epg' | 'spi' | 'catsls' | 'tii' | 'scanner' | 'signal' | 'settings' | 'log' | 'about' | 'stats'

/** A chat message as FM-DX-Webserver sends it (nickname HTML escaped). */
export interface ChatMessage {
  id: number
  nickname: string
  message: string
  /** "YYYY-MM-DD HH:MM", the server's time */
  time: string
  admin?: boolean
  /** address of the sender: for admins only */
  ip?: string
  history?: boolean
}

export interface ChatState {
  messages: ChatMessage[]
  /** own address and admin rights, as the chat server sees them */
  me: { ip: string; admin: boolean } | null
  connected: boolean
  unread: number
}
export type Theme = 'auto' | 'light' | 'dark'
export type Lang = 'pl' | 'en'

export interface SlideProgress {
  bytes: number
  size: number
  name: string
  /** Date.now() of the last update */
  at: number
  /** received completely (the bar fades out) */
  done?: boolean
}

export interface ServiceData {
  dls?: Dls
  slide?: Slide
  /** slide being received */
  slideProgress?: SlideProgress
  /** RadioVIS (slideshow over IP) picture and text */
  visSlide?: Slide
  visText?: string
  format?: AudioFormat
  stats?: AudioStats
}

export interface Notice {
  id: number
  level: string
  message: string
}

export interface SpectrumData {
  frequency: number
  offset: number
  spectrum: Float32Array
  nullSpectrum: Float32Array
  /** false while not synchronised (no null symbol) */
  hasNull?: boolean
  /** DQPSK points (re, im interleaved); empty without synchronisation */
  constellation?: Float32Array
  /** power delay profile in dB, taps from -taps/2 around the start of the FFT window */
  impulse?: Float32Array
  /** window advance and guard interval (samples), noise power of one tap (dB) */
  advance?: number
  guard?: number
  noise?: number
  received: number
}

export function load<T>(key: string, def: T): T {
  try {
    const v = localStorage.getItem('magicdab.' + key)
    return v === null ? def : (JSON.parse(v) as T)
  } catch {
    return def
  }
}

export function save(key: string, v: unknown) {
  try {
    localStorage.setItem('magicdab.' + key, JSON.stringify(v))
  } catch {
    /* storage may be unavailable */
  }
}

/** Session storage, which the browser may block (site data off): nothing is kept then. */
export function sessionGet(key: string): string | null {
  try {
    return sessionStorage.getItem(key)
  } catch {
    return null
  }
}

export function sessionSet(key: string, v: string | null) {
  try {
    if (v === null) sessionStorage.removeItem(key)
    else sessionStorage.setItem(key, v)
  } catch {
    /* storage may be unavailable */
  }
}

export function isLocal() {
  return ['localhost', '127.0.0.1', '[::1]'].includes(location.hostname)
}

const defaultLang: Lang = (navigator.language || 'en').toLowerCase().startsWith('pl') ? 'pl' : 'en'

/** Listing on the public server map (admins). */
export interface ServerMapStatus {
  /** off, unavailable, connecting, listed, error, removing ('' before the start) */
  state: string
  /** why it cannot be listed (unavailable) */
  problem: string | null
  map: string
  defaultMap: string
  /** address listed */
  url: string | null
  id: string | null
  page: string | null
  /** false: registered, the map still checks the address */
  visible: boolean | null
  notice: string | null
  /** Unix times */
  lastOk: number | null
  errorKind: string | null
  error: string | null
  errorAt: number | null
  retryAt: number | null
  /** the station list was last sent */
  stationsAt?: number | null
}

export interface Store {
  connection: 'connecting' | 'open' | 'closed'
  hello: Hello | null
  state: RadioState | null
  signal: Signal | null
  ensemble: Ensemble
  serviceList: EnsembleEntry[]
  time: { utc: number | null; lto: number; at: number }
  tii: TiiState | null
  tiiPairs: number[]
  admin: boolean
  /** token of the admin login for the chat */
  chatToken: string | null
  chat: ChatState
  chatNick: string
  /** the chat window is open */
  chatOpen: boolean
  settings: any
  logLines: LogLine[]
  selected: { sid: number; scids: number } | null
  services: Record<number, ServiceData>
  spectrum: SpectrumData | null
  /** FM tuner, null without one */
  fm: FmState | null
  fmScan: [number, number][]
  linking: LinkingState | null
  /** DAB/FM linking of the main service */
  linkInfo: LinkInfo | null
  /** updates of the program (admins) */
  update: {
    current: string
    platform: string | null
    container: boolean
    url: string
    customUrl: string
    defaultUrl: string
    auto: boolean
    available: { version: string; url: string; notes: string | null } | null
    error: string | null
    busy: boolean
  } | null
  snrHistory: { t: number; snr: number }[]
  notices: Notice[]
  listeners: number
  logos: Record<string, LogoVariant[]>
  texts: Record<string, ServiceText>
  logoUrls: Record<string, string>
  /** loudness of the programme heard: momentary / short-term / integrated
   * LUFS, peak and its maximum since the station was selected (dBFS) */
  meter: { m: number; s: number; i: number; peak: number; max: number } | null
  /** slides of a service seen during the session (Slides view) */
  gallery: { sid: number; slides: GallerySlide[] } | null
  galleryUrls: Record<number, string>
  logoDialog: number | null
  dxResults: DxResult[]
  useRadioDns: boolean
  dlsAll: Record<number, string>
  changeLog: ChangeEvent[]
  epgIndex: Record<string, number>
  epg: Record<number, { start: number; duration: number; name: string; description: string | null; genres: string[] }[]>
  recording: { format: 'aac' | 'opus'; started: number; bytes: number; label: string } | null
  rawRecord: { active: boolean; file?: string; position?: number; length?: number; files?: { name: string; size: number; time: number }[] } | null
  /** DX recordings of multiplexes heard for the first time (admins) */
  dxrec: DxRecState | null
  // audio
  audioEnabled: boolean
  audioPlaying: boolean
  audioBuffer: number
  volume: number
  muted: boolean
  codec: 'opus' | 'pcm'
  // ui
  view: View
  theme: Theme
  lang: Lang
  favorites: number[]
  listMode: 'ensemble' | 'favorites'
  spiDocs: SpiDoc[]
  dataProgress: DataProgress[]
  uiUpdate: boolean
  loginOpen: boolean
  tunnelStatus: { state: string; message: string; url: string | null } | null
  serverMapStatus: ServerMapStatus | null
  loginError: boolean
  audioUnderruns: number
  /** the player's buffer now: the one chosen plus what dropouts added */
  audioTarget: number
  spiDocXml: Record<string, string | null>
  /** may tune while the tuner is locked (gave the tuning password) */
  tuner: boolean
  /** the dialog of the tuning password */
  tuneOpen: boolean
  tuneError: boolean
  /** the connected listeners (admins: the dialog of the status bar) */
  listenersOpen: boolean
  listenerList: { clients: Listener[]; self: number } | null
  /** the server closed the connection on purpose: no reconnecting by itself
   * (idle: no answer to "are you still listening?") */
  closedBy: 'kicked' | 'refused' | 'idle' | null
  /** the gate of a private server: the password being tried (kept for
   * reconnects once accepted), whether the last one was wrong, logins locked */
  gate: { pending: string | null; error: boolean; locked: boolean }
  /** "are you still listening?" asked by the server: seconds to answer, when */
  stillHere: { seconds: number; at: number } | null
  /** statistics of the server (admins) and their last hour */
  stats: Stats | null
  statsHistory: StatsPoint[]
  /** round trip of a ping over the connection (ms); late: no answer for a while */
  latency: { ms: number; late: boolean } | null
  /** the list of keyboard shortcuts is shown */
  shortcutsOpen: boolean
  /** the first-run wizard is shown */
  setupOpen: boolean
  /** the server's background picture is shown in this browser */
  showBackground: boolean
  set: (p: Partial<Store>) => void
  setService: (sid: number, d: Partial<ServiceData>) => void
  toggleFavorite: (sid: number) => void
  notify: (level: string, message: string) => void
}

let noticeId = 0

export const useStore = create<Store>((set, get) => ({
  connection: 'connecting',
  hello: null,
  state: null,
  signal: null,
  ensemble: { valid: false, channel: null, frequency: 0 },
  serviceList: [],
  time: { utc: null, lto: 0, at: 0 },
  tii: null,
  tiiPairs: [],
  admin: false,
  chatToken: null,
  chat: { messages: [], me: null, connected: false, unread: 0 },
  chatNick: load('chatNick', `User ${Math.random().toString(36).slice(2, 7).toUpperCase()}`),
  chatOpen: false,
  settings: null,
  logLines: [],
  // the last station survives reloads (e.g. after a UI update)
  selected: load<{ sid: number; scids: number } | null>('selected', null),
  services: {},
  spectrum: null,
  fm: null,
  fmScan: [],
  linking: null,
  linkInfo: null,
  update: null,
  snrHistory: [],
  notices: [],
  listeners: 0,
  logos: {},
  texts: {},
  logoUrls: {},
  gallery: null,
  galleryUrls: {},
  meter: null,
  logoDialog: null,
  dxResults: [],
  useRadioDns: load('useRadioDns', true),
  recording: null,
  rawRecord: null,
  dxrec: null,
  dlsAll: {},
  changeLog: [],
  epgIndex: {},
  epg: {},
  audioEnabled: false,
  audioPlaying: false,
  audioBuffer: 0,
  volume: load('volume', 0.8),
  muted: false,
  // (the Android app: PCM next to the server spares the phone encoding and decoding Opus)
  codec: isLocal() ? load('codec', androidApp ? 'pcm' : 'opus') : 'opus',
  view: 'service',
  theme: load('theme', 'dark'),
  lang: load('lang', defaultLang),
  favorites: load('favorites', [] as number[]),
  listMode: load('listMode', 'ensemble'),
  spiDocs: [],
  dataProgress: [],
  uiUpdate: false,
  loginOpen: false,
  tunnelStatus: null,
  serverMapStatus: null,
  loginError: false,
  audioUnderruns: 0,
  audioTarget: 0,
  spiDocXml: {},
  tuner: false,
  tuneOpen: false,
  tuneError: false,
  listenersOpen: false,
  listenerList: null,
  closedBy: null,
  gate: { pending: null, error: false, locked: false },
  stillHere: null,
  stats: null,
  statsHistory: [],
  latency: null,
  shortcutsOpen: false,
  setupOpen: false,
  showBackground: load('showBackground', true),
  set: (p) => set(p),
  setService: (sid, d) => set((s) => ({ services: { ...s.services, [sid]: { ...s.services[sid], ...d } } })),
  toggleFavorite: (sid) => {
    const f = get().favorites
    const next = f.includes(sid) ? f.filter((x) => x !== sid) : [...f, sid]
    save('favorites', next)
    set({ favorites: next })
  },
  notify: (level, message) => {
    const id = ++noticeId
    set((s) => ({ notices: [...s.notices, { id, level, message }] }))
    setTimeout(() => set((s) => ({ notices: s.notices.filter((n) => n.id !== id) })), 5000)
  },
}))

const ALL_CHANNELS: string[] = []

/** The channels this client may tune to: a test, true for every one for the
 *  admin or without a limit (Settings > Access). */
export function useChannelAllowed() {
  const allowed = useStore((s) => (s.admin ? ALL_CHANNELS : s.hello?.allowedChannels ?? ALL_CHANNELS))
  return useCallback((channel: string | null | undefined) => !allowed.length || (!!channel && allowed.includes(channel)), [allowed])
}
