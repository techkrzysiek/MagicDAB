// Chat of the listeners: the WebSocket /chat of FM-DX-Webserver, with its messages.
import { useStore, save, type ChatMessage } from './store'

/** Messages kept in the page (the server sends its last 50 at every connect). */
const KEEP = 200

let socket: WebSocket | null = null
let retry = 0
let timer = 0
/** Token the open socket was made with (an admin login gives a new one). */
let socketToken: string | null = null
/** Kicked by an admin or refused: no reconnecting until the page connects again. */
let stopped = false

function url(token: string | null) {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const base = location.pathname.replace(/[^/]*$/, '')
  return `${proto}//${location.host}${base}chat${token ? `?token=${encodeURIComponent(token)}` : ''}`
}

function setChat(p: Partial<ReturnType<typeof useStore.getState>['chat']>) {
  const st = useStore.getState()
  st.set({ chat: { ...st.chat, ...p } })
}

function connect() {
  clearTimeout(timer)
  const st = useStore.getState()
  if (!st.hello?.chat || socket || stopped) return
  socketToken = st.chatToken
  const ws = new WebSocket(url(socketToken))
  socket = ws
  ws.onopen = () => {
    retry = 0
    setChat({ connected: true })
  }
  ws.onmessage = (ev) => {
    if (typeof ev.data !== 'string') return
    let m: Record<string, unknown>
    try {
      m = JSON.parse(ev.data)
    } catch {
      return
    }
    const chat = useStore.getState().chat
    if (m.type === 'clientIp') {
      // a new connection: the history follows
      setChat({ me: { ip: String(m.ip ?? ''), admin: m.admin === true }, messages: [] })
    } else if (m.type === 'clear') {
      setChat({ messages: [] })
    } else if (m.type === 'delete') {
      setChat({ messages: chat.messages.filter((x) => x.id !== Number(m.id)) })
    } else if (typeof m.nickname === 'string' && typeof m.message === 'string') {
      const msg = m as unknown as ChatMessage
      const unread = !msg.history && !useStore.getState().chatOpen ? chat.unread + 1 : chat.unread
      setChat({ messages: [...chat.messages, msg].slice(-KEEP), unread })
    }
  }
  ws.onclose = (ev) => {
    if (socket !== ws) return
    socket = null
    setChat({ connected: false })
    if (ev.code === 4001 || ev.code === 4003) {
      stopped = true
      return
    }
    retry = Math.min(retry + 1, 10)
    timer = window.setTimeout(connect, 1000 * retry)
  }
}

/** The page connected again after a kick: the chat too. */
export function restartChat() {
  stopped = false
  retry = 0
  connect()
}

function disconnect() {
  clearTimeout(timer)
  const ws = socket
  socket = null
  ws?.close()
  setChat({ connected: false })
}

/** Follows the server's chat setting and the admin login (a new token: connect anew). */
export function initChat() {
  useStore.subscribe((s, prev) => {
    if (s.hello?.chat !== prev.hello?.chat) {
      if (s.hello?.chat) connect()
      else disconnect()
    } else if (s.chatToken !== prev.chatToken && socket && s.chatToken !== socketToken) {
      disconnect()
      connect()
    }
    if (s.chatOpen && s.chat.unread > 0) setChat({ unread: 0 })
  })
}

export function sendChat(message: string) {
  const st = useStore.getState()
  // (a generated nickname stays the same from now on)
  save('chatNick', st.chatNick)
  socket?.send(JSON.stringify({ nickname: st.chatNick, message }))
}

export function setChatNick(nick: string) {
  const v = nick.trim().slice(0, 32)
  if (!v) return
  useStore.getState().set({ chatNick: v })
  save('chatNick', v)
}

/** Admin: the whole history, or one message, for everybody. */
export function chatClear() {
  socket?.send(JSON.stringify({ type: 'clear' }))
}

export function chatDelete(id: number) {
  socket?.send(JSON.stringify({ type: 'delete', id }))
}

/** The nickname as it was written (the server escapes it for HTML). */
export function unescapeNick(s: string) {
  const map: Record<string, string> = { '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"', '&#039;': "'" }
  return s.replace(/&(amp|lt|gt|quot|#039);/g, (e) => map[e])
}
