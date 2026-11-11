// Message and model types shared with the MagicDAB server.

export interface ChannelInfo {
  name: string
  frequency: number
}

export interface Hello {
  description?: string
  /** the program can play the sound itself (a page on its computer: speaker.rs) */
  speaker?: boolean
  /** the chat of the listeners is on (/chat) */
  chat?: boolean
  version: string
  clientId: number
  stationName: string
  /** source code of this version (AGPL) */
  sourceUrl?: string
  admin: boolean
  tunerLocked: boolean
  fixedChannel?: string | null
  location: { lat: number; lon: number } | null
  channels: ChannelInfo[]
  txdb: { count: number; updated: number | null }
  opusBitrate: number
  /** a tuning password exists (trusted listeners tune while the tuner is locked) */
  tunePassword?: boolean
  /** this client gave it */
  tuner?: boolean
  /** channels listeners may tune to; empty: all (the admin is not limited) */
  allowedChannels?: string[]
  /** listeners tune the mapped channels on receivers of their own (the main one stays) */
  listenerMultiplex?: boolean
  /** picture behind the interface (Settings > Webserver), empty: none */
  backgroundImage?: string
  /** a new installation not set up yet (first-run wizard) */
  setupPending?: boolean
  /** a private server: visitors give the administrator or the tuning password
   * first (until then this hello is all they get) */
  private?: boolean
}

/** A connection as the server lists it for admins. */
export interface Listener {
  id: number
  address: string
  agent: string
  /** public host it came through (the tunnel), empty when local */
  via: string
  connectedSecs: number
  admin: boolean
  tuner: boolean
  service: number | null
  /** "Label (SId)" */
  station: string | null
  /** channel it listens to (its own multiplex or the main receiver's) */
  channel?: string | null
  audio: boolean
  /** network of the address (Team Cymru), when looked up */
  asn: { asns: number[]; prefix: string; country: string } | null
  /** waits at the gate of a private server (gave no password yet) */
  gated?: boolean
}

/** A banned address or network (Settings > Access). */
export interface Ban {
  address: string
  reason: string
  since: number
  until: number | null
  auto: boolean
}

export interface DeviceStatus {
  kind: string
  name: string
  connected: boolean
  frequency: number
  gainDb: number | null
  gainIndex: number | null
  gains: number[]
  rfLevel: number | null
  error: string | null
  position: number | null
  duration: number | null
  stalls?: number
  lastStallMs?: number | null
  streamRate?: number | null
  /** the device works but will hear nothing (magicdab-sdr DeviceStatus::warning) */
  warning?: 'v4_library' | null
}

export interface ScanState {
  running: boolean
  silent: boolean
  index: number
  total: number
  channel: string | null
  found: number
  dx: { mode: string; cycle: number; cycles: number } | null
}

export interface RadioState {
  channel: string | null
  frequency: number
  device: DeviceStatus
  /** why the tuner does not work (radio/outgoing.rs device_problem), null when it does */
  deviceProblem?: 'access' | 'busy' | 'missing' | 'driver' | 'unzip' | 'library' | 'other' | null
  deviceConfig: any
  gain: any
  scan: ScanState | null
  listeners: number
  tunerLocked: boolean
  fixedChannel?: string | null
  mainService: number | null
  input?: 'rf' | 'eti'
  etiConnected?: boolean | null
  mappedChannels?: string[]
  noTuner?: boolean
  /** The computer cannot keep up with the receiver: % of the input lost, the receiver's CPU (% of a processor). */
  overload?: { lost: number; cpu?: number | null } | null
  /** this listener's receiver: 0 the main one, else one of its own multiplex */
  slot?: number
  /** channel of the main receiver */
  mainChannel?: string | null
  /** the DX recording played back on this receiver */
  playback?: string | null
  playbackInfo?: PlaybackInfo | null
}

/** A DX recording played back: what it is (radio/slots.rs). */
export interface PlaybackInfo {
  name: string
  label: string
  channel: string
  ecc: string
  eid: string
  /** start of the recording (unix) */
  time: number
  duration: number | null
  /** TII, spectrum and signal were recorded with it */
  side: boolean
}

/** A DX recording (an ETI file of a multiplex heard for the first time), admins */
export interface DxRecording {
  name: string
  time: number
  channel: string
  frequency: number
  ecc: string
  eid: string
  label: string
  services: { sid: string; label: string; bitrate: number; codec: string }[]
  snr: number
  snrMean: number
  tii: string[]
  duration: number
  size: number
  frames: number
  reason: string
  ended: string
  /** TII, spectrum and signal recorded with it */
  side?: boolean
}

export interface DxRecState {
  active: { channel: string; label: string; eid: string; reason: string; seconds: number; limit: number; bytes: number; synced: boolean } | null
  enabled: boolean
  minutes: number
  maxMb: number
  uploadMaxMb: number
  recordings?: DxRecording[]
  usedMb?: number
}

export interface Signal {
  /** 'file': a DX recording played back */
  input?: string
  /** a DX recording: the signal is the one recorded with it */
  recorded?: boolean
  /** DTR-100: reception as the dongle reports it (0..4) and its channel error rate. */
  bars?: number
  cer?: number | null
  synced: boolean
  signal: boolean
  snr: number
  freqOffset: number
  fibErrorRate: number
  /** FIC bit error ratio before the Viterbi decoder */
  ficBer?: number
  fibTotal: number
  fibErrors: number
  gain: number | null
  gainIndex: number | null
  rfLevel: number | null
  position: number | null
  duration: number | null
}

export interface UserApp {
  type: number
  name: string
  xpadAppType: number | null
  dscty: number | null
  data: string
}

export interface Component {
  scids: number | null
  tmid: number
  type: string
  ctype: number
  subch: number | null
  scid: number | null
  primary: boolean
  ca: boolean
  label: string | null
  short: string | null
  long: string | null
  language: string | null
  packetAddress: number | null
  bitrate: number | null
  protection: string | null
  userApps: UserApp[]
}

export interface FreqRef {
  khz: number
  channel: string | null
}

export interface Linking {
  /** FIG 0/6 service linking */
  links: { kind: string; id: string; label: string | null; ecc: string | null; lsn: string; hard: boolean; active: boolean; international: boolean; oe: boolean; cached?: boolean }[]
  /** FIG 0/21 frequency information */
  frequencies: { kind: string; ref: string; frequencies: FreqRef[]; cached?: boolean }[]
  /** FIG 0/24 other ensembles */
  ensembles: { eid: string; via: string; frequencies: FreqRef[]; cached?: boolean }[]
}

export interface Service {
  sid: number
  sidHex: string
  ecc: number | null
  country: string | null
  countryName: string | null
  label: string
  short: string
  long: string | null
  pty: number | null
  ptyName: string | null
  ptyDynamic: boolean
  language: string | null
  isAudio: boolean
  isData: boolean
  ca: boolean
  codec: string | null
  ascty: number | null
  bitrate: number | null
  subchannel: number | null
  protection: string | null
  codeRate: string | null
  startCu: number | null
  sizeCu: number | null
  components: Component[]
  asu: string[]
  clusters: number[]
  linking: Linking
  /** fields taken from the FIG cache (pty, language, asu, long) */
  cachedFields?: string[]
}

export interface Subchannel {
  id: number
  start: number
  size: number
  bitrate: number
  protection: string
  codeRate: string
  fec: number
  audio: boolean
  content: string | null
  services: string[]
}

export interface FrequencyInfo {
  id: string
  rm: number
  type: string
  oe: boolean
  regionId: number
  continuity: boolean
  frequencies: { khz: number; channel: string | null; adjacent: boolean | null; mode: number | null }[]
  label: string | null
  cached?: boolean
}

export interface Linkage {
  lsn: string
  active: boolean
  hard: boolean
  international: boolean
  oe: boolean
  keySid: string | null
  keyLabel: string | null
  ids: { kind: string; id: string; ecc: string | null }[]
  cached?: boolean
}

export interface Ensemble {
  valid: boolean
  channel: string | null
  frequency: number
  eid?: number
  eidHex?: string
  ecc?: number | null
  eccHex?: string | null
  ueid?: string | null
  country?: string | null
  countryName?: string | null
  label?: string
  short?: string
  long?: string | null
  lto?: number | null
  interTableId?: number | null
  alarm?: boolean
  services?: Service[]
  subchannels?: Subchannel[]
  usedCu?: number
  freeCu?: number
  audioCu?: number
  dataCu?: number
  frequencyInfo?: FrequencyInfo[]
  linkage?: Linkage[]
  oeServices?: { sid: string; label: string | null; ensembles: { eid: string; frequencies: number[]; channels: string[] }[]; cached?: boolean }[]
  /** unix time of the FIG cache when cached items are included */
  cacheTime?: number
  longCached?: boolean
  announcements?: { cluster: number; types: string[]; subch: number; newFlag: boolean; region: number | null }[]
}

export interface ServiceEntry {
  sid: number
  scids: number
  label: string
  short: string
  pty: number | null
  codec: string
  bitrate: number
  ecc: number | null
}

export interface EnsembleEntry {
  channel: string
  frequency: number
  eid: number
  ecc: number
  label: string
  short: string
  services: ServiceEntry[]
  lastSeen: number
  snr: number
  tii: string[]
}

export interface DlPlusTag {
  contentType: number
  name: string
  text: string
}

export interface Dls {
  text: string
  tags: DlPlusTag[]
  itemToggle: boolean
  itemRunning: boolean
}

export interface AudioFormat {
  codec: string
  sampleRate: number
  channels: number
  sbr: boolean
  ps: boolean
  stereo: boolean
  bitrate: number
}

export interface AudioStats {
  frames: number
  frameErrors: number
  auOk: number
  auErrors: number
  rsCorrectedBytes: number
  rsCorrectedBits: number
  rsUncorrectable: number
  rsCodewords: number
  superframes: number
  audioBytes: number
  padBytes: number
  decodeErrors: number
}

/** FM tuner (FM-DX-Tuner) with RDS decoded by the server (librdsparser). */
export interface FmRds {
  pi: number | null
  ps: string
  rt: string
  pty: number | null
  ptyName: string
  tp: boolean | null
  ta: boolean | null
  music: boolean | null
  ecc: number | null
  country: string
  countryIso: string
  af: number[]
  ptyn: string
  lps: string
}

export interface FmState {
  connected: boolean
  port: string
  khz: number
  antenna: number
  antennas: number
  signal: { stereo: boolean; level: number; cci: number; aci: number }
  piErrors: number | null
  rds: FmRds
}

/** Short linking state of the main service (all listeners). */
export interface LinkInfo {
  sid: number | null
  enabled?: boolean
  mode?: 'auto' | 'dab' | 'fm'
  phase?: string
  khz?: number | null
  antenna?: number | null
  antennas?: number
  progress?: { done?: number; total?: number; left?: number } | null
  targets?: string[]
  aligned?: boolean
  source?: 'dab' | 'fm' | null
  delay?: number | null
  reason?: string
}

/** Debug state of the DAB/FM linking of the service heard. */
export interface LinkingState {
  enabled: boolean
  mode: 'auto' | 'dab' | 'fm'
  reason: string
  service: { sid: string; label: string; ecc: string | null } | null
  targets: { pi: string; source: string }[]
  phase: { name: string; khz?: number; antenna?: number; for?: number; done?: number; total?: number; left?: number }
  candidates: {
    khz: number
    sources: string[]
    best: boolean
    check: {
      result: string
      ago: number
      measures: { antenna: number; level: number; cci: number; aci: number; pi: string | null; score: number | null }[]
    } | null
  }[]
  stations: { khz: number; pi: string | null; level: number; ago: number; match: boolean }[]
  fm: { connected: boolean; khz: number; antenna: number; antennas: number; level: number; cci: number; aci: number; pi: string | null; piErrors: number | null; ps: string }
  dab: { synced: boolean; snr: number; auErrorRate: number; good: boolean; bad: boolean }
  linker: {
    source: 'dab' | 'fm' | null
    fade: number
    aligned: boolean
    dabAligned: boolean
    pacedByFm: boolean
    dabBufferMs: number
    fmDelayMs: number
    /** Median of the last measurements; negative: FM comes later than DAB. */
    fmAheadMs?: number | null
    /** DAB waits this much longer for a late FM. */
    dabExtraMs?: number
    driftPpm: number
    gainDb: number
    polarity: number
    /** Polarity of the FM side signal (L-R); differing from `polarity`: channels swapped. */
    sidePolarity?: number
    stalls: number
    ab: boolean
    /** In A/B listening FM is on the right now (else DAB alone, FM not aligned yet). */
    abFm?: boolean
    /** FM plays without DAB to align it to (DAB has no audio). */
    fmOnly?: boolean
  } | null
  measurement: {
    delayMs: number
    confidence: number
    gainDb: number
    polarity: number
    correlation: number
    /** Above about 1 kHz (tells the polarity), below 250 Hz, side signal above 1 kHz. */
    correlationHigh?: number
    correlationLow?: number
    correlationSide?: number
    curve: number[]
    overview: number[]
    accepted: boolean
  } | null
  history: [number, number, number, boolean][]
  events: [number, string][]
}

export interface Slide {
  url: string
  contentName: string
  mime: string
  size: number
  categoryTitle: string | null
  clickThroughUrl: string | null
  received: number
}

export interface Transmitter {
  country: string
  channel: string
  label: string
  eid: number
  main: number
  sub: number
  location: string
  lat: number
  lon: number
  altitude: number
  height: number
  polarization: string
  frequency: number
  powerKw: number
}

export interface TiiCode {
  main: number
  sub: number
  code: string
  level: number
  margin: number
  tx: Transmitter | null
  distance: number | null
  azimuth: number | null
}

export interface TiiState {
  codes: TiiCode[]
  known: { code: string; location: string; lat: number; lon: number; power: number; frequency: number }[]
  eid: string
  frequency: number
}

export interface LogLine {
  time: string
  level: string
  target: string
  message: string
}

export interface LogoVariant {
  id: string
  sid: number
  ecc: number
  width: number
  height: number
  mime: string
  source: string
  size: number
  received: number
  url: string
}

export interface ServiceText {
  shortName: string | null
  mediumName: string | null
  longName: string | null
  description: string | null
  links: string[]
}

export interface DxResult {
  time: number
  channel: string
  frequency: number
  ueid: string
  eid: number
  label: string
  services: number
  snr: number
  main: number | null
  sub: number | null
  level: number | null
  location: string | null
  lat: number | null
  lon: number | null
  power: number | null
  distance: number | null
  azimuth: number | null
  country: string | null
}

export interface Change {
  kind: string
  service: string | null
  sid: string | null
  from: string | null
  to: string | null
}

export interface ChangeEvent {
  time: number
  channel: string
  eid: string
  ensemble: string
  changes: Change[]
  report?: ChangeReport
  /** the DX recording a new station or another line-up started */
  recording?: string
}

// the first minute of a new multiplex (radio/newmux.rs)
export interface ChangeReport {
  seconds: number
  ended?: string
  snrMean: number
  snrMax: number
  fibErrors: number
  country?: string
  transmitters: ChangeReportTx[]
  recording?: string
}

export interface ChangeReportTx {
  code: string
  level: number
  location: string | null
  country: string | null
  distance: number | null
  azimuth: number | null
  powerKw: number | null
}

/** What the SPI diagnostics found in a carousel (server: spi.rs, dab mot.rs). */
export interface SpiIssue {
  /** 2 may hang receivers, 1 against the standard, 0 unusual */
  level: 0 | 1 | 2
  code: string
  subject: string
  a?: number
  b?: number
  text?: string
}

export interface SpiDoc {
  id: string
  source: string
  kind: string
  url: string
  scope: string
  received: number
  size: number
  items: number
}

export interface DataProgress {
  sid: string
  scid: number
  ua: number
  label: string | null
  objects: number
  objectsDone: number
  bytes: number
  bytesDone: number
  segments?: number
  segmentsTotal?: number
}

/** Statistics of the server (admins, the Statistics view); CPU in % of one
 *  processor unless said otherwise, rates in bytes/s. */
export interface StatsReceiver {
  slot: number
  channel: string
  input: 'stream' | 'recording' | 'rtlsdr' | 'rtltcp' | 'dongle' | 'none'
  cpu?: number
  ofdm?: number
  fic?: number
  background?: number
  data?: number
  tii?: number
  /** the DX recording: the sub-channels nothing else decodes */
  recording?: number
  services?: { sid: number; label: string; channel: number; audio: number; opus: number; listeners: number }[]
}

export interface Stats {
  uptime: number
  version: string
  machine: { cpuModel: string; cores: number; os: string }
  /** the whole computer: % of all processors */
  system: { cpu: number | null; ramTotal: number | null; ramUsed: number | null; uptime?: number | null }
  process: { cpu: number | null; ram: number | null; threads: number }
  listeners: { connected: number; listening: number; admins?: number; pcm?: number }
  net: { out: number; sent: number; tunnel?: number; local?: number; inputs?: { name: string; rate: number }[] }
  receivers?: StatsReceiver[]
  threads?: { name: string; count: number; cpu: number }[]
}

/** [unix time, CPU of the computer %, CPU of MagicDAB (% of one processor), bytes/s out] */
export type StatsPoint = [number, number | null, number | null, number]
