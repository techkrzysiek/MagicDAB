// The sound of a station of the main receiver through /audio/<SId>
// (crates/server/src/main.rs: audio_socket_handler): the same bytes for every
// listener, so a proxy in front of the server (the tunnel's) passes one copy
// to all of them. The page still says over /ws that it listens (codec
// "shared"): the server decodes the station and counts the listener as before.
// Where /audio does not work (an older server, a proxy that does not pass it)
// the sound goes over /ws again for the rest of the visit.
import { useStore } from '../store'
import type { Store } from '../store'
import { send } from '../ws'
import { player } from './player'

/** FM (DAB/FM linking) is no station of the multiplex (actions.ts FM_SID). */
const FM_SID = 0xffffffff

let socket: WebSocket | null = null
let socketSid: number | null = null
/** /audio failed on this visit: the sound over /ws. */
let failed = false
let retry: ReturnType<typeof setTimeout> | null = null
/** The codec last told the server, to tell it again when it changes. */
let toldCodec: string | null = null

/** The station plays through /audio: one of the main receiver, Opus, where the server has it. */
export function sharedWanted(s: Store = useStore.getState()): boolean {
  return (
    !failed &&
    !!s.hello?.audioSocket &&
    s.codec === 'opus' &&
    !s.state?.slot &&
    !!s.selected &&
    s.selected.scids === 0 &&
    s.selected.sid !== FM_SID
  )
}

function audioUrl(sid: number) {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const base = location.pathname.replace(/[^/]*$/, '')
  return `${proto}//${location.host}${base}audio/${sid.toString(16).toUpperCase()}`
}

function close() {
  const s = socket
  socket = null
  socketSid = null
  if (s) {
    s.onclose = s.onmessage = s.onopen = null
    s.close()
  }
}

function open(sid: number) {
  const ws = new WebSocket(audioUrl(sid))
  ws.binaryType = 'arraybuffer'
  let opened = false
  socket = ws
  socketSid = sid
  ws.onopen = () => {
    opened = true
  }
  ws.onmessage = (ev) => {
    if (ev.data instanceof ArrayBuffer) player.packet(new Uint8Array(ev.data))
  }
  ws.onclose = () => {
    if (socket !== ws) return
    socket = null
    socketSid = null
    if (!opened) {
      // (not there: the sound over /ws from now on)
      console.warn('/audio not available, the sound goes over /ws')
      failed = true
      tell(useStore.getState())
      return
    }
    // dropped: again in a moment
    retry ??= setTimeout(() => {
      retry = null
      sync()
    }, 1000)
  }
}

/** The codec of the sound to the server when it changed (listening only). */
function tell(s: Store) {
  if (!s.audioEnabled || s.connection !== 'open' || !s.hello) return
  const codec = s.hello.speaker && s.nativeAudio ? 'native' : sharedWanted(s) ? 'shared' : s.codec
  if (codec === toldCodec) return
  toldCodec = codec
  send({ type: 'audio', enabled: true, codec })
}

/** The socket of /audio as the state wants it: the station listened to, or none. */
export function sync() {
  const s = useStore.getState()
  const want = s.audioEnabled && s.connection === 'open' && sharedWanted(s) ? s.selected!.sid : null
  if (want !== socketSid) {
    close()
    if (want !== null) open(want)
  }
}

/** What ws.ts and actions.ts told the server, so `tell` compares with it. */
export function told(codec: string | null) {
  toldCodec = codec
}

useStore.subscribe((s, prev) => {
  if (s.connection !== 'open') toldCodec = null
  if (
    s.audioEnabled !== prev.audioEnabled ||
    s.connection !== prev.connection ||
    s.selected !== prev.selected ||
    s.state?.slot !== prev.state?.slot ||
    s.hello !== prev.hello ||
    s.codec !== prev.codec
  ) {
    // (a new connection: ws.ts tells it the codec itself)
    if (prev.connection === 'open') tell(s)
    sync()
  }
})
