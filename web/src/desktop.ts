/**
 * The program's own window on Windows (WebView2): it gives the page a token
 * to log in as admin and talks with it through `window.ipc` (closing the
 * window, the play state for the tray icon).
 *
 * The Android app gives its WebView `MagicDABAndroid` for the same:
 * the token, messages to the app (play state, the station for its
 * notification, the tuner) and saving files.
 */
const host = (window as unknown as { __MAGICDAB_DESKTOP__?: { token: string; serverMode?: boolean } }).__MAGICDAB_DESKTOP__

interface AndroidBridge {
  token(): string
  postMessage(json: string): void
  /** A file in the phone's Downloads, written in parts: its id, 0 when it cannot be made. */
  saveStart(name: string, mime: string): number
  saveChunk(id: number, base64: string): boolean
  saveEnd(id: number, ok: boolean): void
}
const android = (window as unknown as { MagicDABAndroid?: AndroidBridge }).MagicDABAndroid

/** The program starts as a server, without the window (kept by the program). */
export const desktopServerMode = !!host?.serverMode

export const desktop = !!host
/** The page runs in the Android app. */
export const androidApp = !!android
export const desktopToken = host?.token ?? android?.token()

/** A message to the window (or the Android app). */
export function toHost(msg: Record<string, unknown>) {
  const json = JSON.stringify(msg)
  ;(window as unknown as { ipc?: { postMessage(s: string): void } }).ipc?.postMessage(json)
  android?.postMessage(json)
}

/** The content of a blob as a data: address. */
export function dataUrl(blob: Blob): Promise<string> {
  return new Promise((ok, fail) => {
    const r = new FileReader()
    r.onload = () => ok(String(r.result))
    r.onerror = () => fail(r.error)
    r.readAsDataURL(blob)
  })
}

/**
 * Android: a file goes to the phone's Downloads through the app (a WebView
 * saves no downloads); false outside the app.
 */
export function saveInApp(name: string, blob: Blob): boolean {
  if (!android) return false
  const bridge = android
  void (async () => {
    const id = bridge.saveStart(name, blob.type || 'application/octet-stream')
    if (!id) return
    let ok = false
    try {
      // in parts (a recording can be large), a multiple of 3 bytes each
      const part = 3 << 18
      for (let at = 0; at < blob.size; at += part) {
        const url = await dataUrl(blob.slice(at, at + part))
        if (!bridge.saveChunk(id, url.slice(url.indexOf(',') + 1))) return
      }
      ok = true
    } catch {
      /* the app tells that the file was not saved */
    } finally {
      bridge.saveEnd(id, ok)
    }
  })()
  return true
}

/** Stored choices of the window (this computer only). */
export const closePrefs = {
  get action(): 'ask' | 'hide' | 'quit' | 'server' {
    try {
      const v = localStorage.getItem('magicdab.closeAction')
      return v === 'hide' || v === 'quit' || v === 'server' ? v : 'ask'
    } catch {
      return 'ask'
    }
  },
  set action(v: 'ask' | 'hide' | 'quit' | 'server') {
    try {
      localStorage.setItem('magicdab.closeAction', v)
    } catch {
      /* storage unavailable */
    }
  },
}
