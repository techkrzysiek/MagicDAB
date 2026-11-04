// The page in the Android app: what the app's
// native side needs from it besides the media controls (audio/mediasession.ts).
import { useStore } from './store'
import { androidApp, toHost } from './desktop'

export function initAndroid() {
  if (!androidApp) return
  // the play state (notification, audio focus, staying awake) and the tuner:
  // the app starts the "RTL-SDR driver" app again when the stick is gone
  let playing = false
  let device = ''
  useStore.subscribe((s) => {
    if (s.audioEnabled !== playing) {
      playing = s.audioEnabled
      toHost({ type: 'playing', value: playing })
    }
    const d = s.state?.device
    const key = d ? `${d.kind}|${d.connected}` : ''
    if (d && key !== device) {
      device = key
      toHost({ type: 'device', kind: d.kind, connected: d.connected })
    }
  })
  // the back button first closes what is open on the page (dialogs, menus,
  // the chat): true when it did
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
    return false
  }
}
