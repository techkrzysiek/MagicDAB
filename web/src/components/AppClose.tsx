import { useEffect, useState } from 'react'
import { X } from 'lucide-react'
import { useT } from '../i18n'
import { androidApp, closePrefs, toHost } from '../desktop'

/** Event that opens the question (android.ts: the back button; the More menu). */
export const APP_CLOSE_EVENT = 'magicdab-app-close'

/**
 * Android app: the back button (with nothing open on the page) and More >
 * Quit MagicDAB ask whether to leave it playing in the background or to quit
 * it (the receiver, the sound, the app; android/…/MainActivity.kt "close").
 */
export function AppClose() {
  const t = useT()
  const [open, setOpen] = useState(false)
  const [remember, setRemember] = useState(false)

  useEffect(() => {
    if (!androidApp) return
    const onOpen = () => {
      setRemember(false)
      setOpen(true)
    }
    window.addEventListener(APP_CLOSE_EVENT, onOpen)
    return () => window.removeEventListener(APP_CLOSE_EVENT, onOpen)
  }, [])

  if (!androidApp || !open) return null
  const choose = (action: 'hide' | 'quit' | 'cancel') => {
    setOpen(false)
    if (action === 'cancel') return
    if (remember) closePrefs.action = action
    toHost({ type: 'close', action })
  }
  return (
    <div className="modal-back" onClick={() => choose('cancel')}>
      <div className="modal close-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <b>{t('close_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={() => choose('cancel')} aria-label="close">
            <X />
          </button>
        </div>
        <div className="close-options">
          {(['hide', 'quit'] as const).map((a) => (
            <button key={a} type="button" className={'close-opt' + (a === 'quit' ? ' primary' : '')} onClick={() => choose(a)}>
              <b>{t(`app_close_${a}`)}</b>
              <span>{t(`app_close_${a}_hint`)}</span>
            </button>
          ))}
        </div>
        <div className="close-actions">
          <label className="check">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} /> {t('app_close_remember')}
          </label>
          <span style={{ flex: 1 }} />
          <button type="button" className="btn" onClick={() => choose('cancel')}>
            {t('cancel')}
          </button>
        </div>
      </div>
    </div>
  )
}
