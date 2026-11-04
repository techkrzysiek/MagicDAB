// Integration with the operating system's media controls (Android media
// notification, lock screen, headset buttons). Browsers only offer them while
// an <audio> element plays; the radio itself plays through Web Audio, so a
// looping silent element keeps the media session alive (and the page running
// in the background on phones). The Android app has a notification
// and audio focus of its own: it gets the station and sends the actions.
import { useStore } from '../store'
import { pickLogo } from '../components/logo'
import { requestLogo } from '../ws'
import { findService } from '../util'
import { FM_SID } from '../actions'
import { androidApp, dataUrl, toHost } from '../desktop'

let element: HTMLAudioElement | null = null

/**
 * Ten seconds of 8 kHz 8 bit silence as a WAV file: Chrome shows the media
 * notification only for media of 5 s or more (shorter ones count as sounds).
 */
function silentWav(): string {
  const n = 8000 * 10
  const b = new Uint8Array(44 + n)
  const v = new DataView(b.buffer)
  const str = (o: number, s: string) => [...s].forEach((c, i) => (b[o + i] = c.charCodeAt(0)))
  str(0, 'RIFF')
  v.setUint32(4, 36 + n, true)
  str(8, 'WAVEfmt ')
  v.setUint32(16, 16, true)
  v.setUint16(20, 1, true) // PCM
  v.setUint16(22, 1, true) // mono
  v.setUint32(24, 8000, true)
  v.setUint32(28, 8000, true)
  v.setUint16(32, 1, true)
  v.setUint16(34, 8, true)
  str(36, 'data')
  v.setUint32(40, n, true)
  b.fill(0x80, 44)
  return URL.createObjectURL(new Blob([b], { type: 'audio/wav' }))
}

/**
 * Live radio has no length: without this, phones show the ten seconds of the
 * silent element as a progress bar running round and round.
 */
function setLive() {
  if (!('mediaSession' in navigator) || typeof navigator.mediaSession.setPositionState !== 'function') return
  try {
    navigator.mediaSession.setPositionState({ duration: Infinity, playbackRate: 1, position: 0 })
  } catch {
    /* a browser that wants a finite length */
  }
}

/** Start the media session; call from the user gesture that starts playback. */
export function mediaStart() {
  // (in the app the element would take the audio focus from the app itself)
  if (!androidApp) startElement()
  lastKey = ''
  updateMetadata()
}

function startElement() {
  if (!element) {
    element = new Audio(silentWav())
    element.loop = true
    element.setAttribute('playsinline', '')
    element.addEventListener('playing', setLive)
  }
  element.play().catch(() => undefined)
  if ('mediaSession' in navigator) navigator.mediaSession.playbackState = 'playing'
  setLive()
}

export function mediaStop() {
  element?.pause()
  if ('mediaSession' in navigator) navigator.mediaSession.playbackState = 'paused'
}

let lastKey = ''

const browserSession = () => 'mediaSession' in navigator && typeof MediaMetadata !== 'undefined'

interface Meta {
  title: string
  artist: string
  album: string
  artwork?: MediaImage
}

/** The station on the media controls: the browser's, or the notification of the Android app. */
function show(m: Meta) {
  if (browserSession()) navigator.mediaSession.metadata = new MediaMetadata({ title: m.title, artist: m.artist, album: m.album, artwork: m.artwork ? [m.artwork] : [] })
  if (androidApp) toApp(m)
}

let appSeq = 0

/** The app gets the logo itself: it cannot read the blob: addresses of the page. */
function toApp(m: Meta) {
  const seq = ++appSeq
  const send = (art?: string) => {
    if (seq === appSeq) toHost({ type: 'media', title: m.title, artist: m.artist, album: m.album, art })
  }
  if (!m.artwork) return send()
  fetch(m.artwork.src)
    .then((r) => r.blob())
    .then(dataUrl)
    .then(send, () => send())
}

function updateMetadata() {
  if (!browserSession() && !androidApp) return
  const s = useStore.getState()
  if (s.selected?.sid === FM_SID) {
    // FM: the RDS name (or the frequency) and the radio text
    const fm = s.fm
    const title = fm?.rds.ps.trim() || (fm ? `${(fm.khz / 1000).toFixed(1)} MHz` : 'FM')
    const key = ['fm', title, fm?.rds.rt].join('|')
    if (key === lastKey) return
    lastKey = key
    show({ title, artist: fm?.rds.rt ?? '', album: 'FM' })
    return
  }
  const svc = findService(s.ensemble, s.selected?.sid)
  if (!svc) return
  const dls = s.services[svc.sid]?.dls?.text ?? s.dlsAll[svc.sid] ?? ''
  const all = s.logos[svc.sid.toString(16).toUpperCase()]
  const variants = s.useRadioDns ? all : all?.filter((v) => v.source !== 'RadioDNS')
  const logo = pickLogo(variants, 160)
  const url = logo ? s.logoUrls[logo.id] : undefined
  if (logo && !url) requestLogo(logo.id)
  const key = [svc.sid, svc.label, dls, s.ensemble.label, url].join('|')
  if (key === lastKey) return
  lastKey = key
  show({
    title: svc.long || svc.label,
    artist: dls,
    album: `${s.ensemble.label ?? ''} · ${s.ensemble.channel ?? ''}`,
    artwork: url && logo ? { src: url, sizes: `${logo.width}x${logo.height}`, type: logo.mime } : undefined,
  })
}

/** Install the action handlers and follow station, DLS and logo changes. */
export function initMediaSession(actions: { play: () => void; stop: () => void; step: (dir: 1 | -1) => void }) {
  // the Android app's notification, lock screen, headset and audio focus
  if (androidApp) {
    window.addEventListener('magicdab-media', (e) => {
      const a = (e as CustomEvent<string>).detail
      if (a === 'play') actions.play()
      else if (a === 'stop') actions.stop()
      else if (a === 'next' || a === 'previous') actions.step(a === 'next' ? 1 : -1)
    })
  } else if (!('mediaSession' in navigator)) return
  if ('mediaSession' in navigator) {
    const ms = navigator.mediaSession
    const set = (a: MediaSessionAction, h: MediaSessionActionHandler) => {
      try {
        ms.setActionHandler(a, h)
      } catch {
        /* action not supported */
      }
    }
    set('play', () => actions.play())
    set('pause', () => actions.stop())
    set('stop', () => actions.stop())
    set('previoustrack', () => actions.step(-1))
    set('nexttrack', () => actions.step(1))
  }
  useStore.subscribe((s, prev) => {
    if (
      s.selected !== prev.selected ||
      s.services !== prev.services ||
      s.dlsAll !== prev.dlsAll ||
      s.logoUrls !== prev.logoUrls ||
      s.logos !== prev.logos ||
      s.ensemble !== prev.ensemble ||
      s.fm !== prev.fm
    ) {
      if (s.audioEnabled) updateMetadata()
    }
  })
}
