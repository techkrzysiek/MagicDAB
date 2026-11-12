// The page in the Android app: what the app's
// native side needs from it besides the media controls (audio/mediasession.ts).
import { useStore } from './store'
import { androidApp, closePrefs, toHost } from './desktop'
import { APP_CLOSE_EVENT } from './components/AppClose'

export function initAndroid() {
  if (!androidApp) return
  // the play state (notification, audio focus, staying awake), a scan (the
  // receiver is not stopped for want of a listener while it runs) and the tuner
  let playing = false
  let scanning = false
  let device = ''
  // the app's loading cover goes once the page shows the multiplex (or the
  // receiver's state a while, when there is none yet: a new installation)
  let ready = false
  let readyTimer = 0
  const sendReady = () => {
    if (ready) return
    ready = true
    // (under the app's cover the page is hidden: no animation frames then)
    if (document.hidden) setTimeout(() => toHost({ type: 'ready' }), 0)
    else requestAnimationFrame(() => requestAnimationFrame(() => toHost({ type: 'ready' })))
  }
  useStore.subscribe((s) => {
    if (!ready && s.hello && s.state) {
      if (s.ensemble.services?.length) sendReady()
      else if (!readyTimer) readyTimer = window.setTimeout(sendReady, 2000)
    }
    if (s.audioEnabled !== playing) {
      playing = s.audioEnabled
      toHost({ type: 'playing', value: playing })
    }
    if (!!s.state?.scan !== scanning) {
      scanning = !!s.state?.scan
      toHost({ type: 'scanning', value: scanning })
    }
    const d = s.state?.device
    const key = d ? `${d.kind}|${d.connected}` : ''
    if (d && key !== device) {
      device = key
      toHost({ type: 'device', kind: d.kind, connected: d.connected })
    }
  })
  // how far an update of the app is (Updater.kt): the banner and Settings show it
  window.addEventListener('magicdab-apk', (e) => {
    useStore.getState().set({ apk: (e as CustomEvent).detail })
  })
  // the back button first closes what is open on the page (dialogs, menus,
  // the chat), then asks: in the background or quit (AppClose; or what was
  // remembered there): true when the page took it
  ;(window as unknown as { magicdabBack?: () => boolean }).magicdabBack = () => {
    const dialogs = document.querySelectorAll<HTMLElement>('.modal-back')
    if (dialogs.length) {
      dialogs[dialogs.length - 1].click()
      return true
    }
    if (document.querySelector('.menu, .dropdown-menu, .chat-popup')) {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      return true
    }
    const a = closePrefs.action
    if (a === 'hide' || a === 'quit') toHost({ type: 'close', action: a })
    else window.dispatchEvent(new Event(APP_CLOSE_EVENT))
    return true
  }
}
