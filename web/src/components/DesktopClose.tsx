import { useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { closePrefs, desktop, toHost } from '../desktop'
import { startAudio, toggleAudio } from '../actions'

/**
 * Windows application: closing the window asks whether to hide it (the
 * receiver keeps serving the network, and playing here if wanted) or to quit;
 * the tray icon plays and stops. Plays at start if it played when closed.
 */
export function DesktopClose() {
  const t = useT()
  const [open, setOpen] = useState(false)
  const [remember, setRemember] = useState(false)
  const audioEnabled = useStore((s) => s.audioEnabled)
  const ready = useStore((s) => s.connection === 'open' && !!s.hello && !!s.selected)
  const resumed = useRef(false)

  // (hidden, it goes on as it was: playing or not; the tray icon plays and stops)
  const act = (action: 'hide' | 'quit' | 'server') => toHost({ type: 'close', action })

  useEffect(() => {
    if (!desktop) return
    const onClose = () => {
      const a = closePrefs.action
      if (a === 'ask') {
        setOpen(true)
        // the page asks: no Windows dialog on top of it after the timeout
        toHost({ type: 'close', action: 'asking' })
      } else act(a)
    }
    const onToggle = () => toggleAudio()
    window.addEventListener('magicdab-close', onClose)
    window.addEventListener('magicdab-toggle-play', onToggle)
    return () => {
      window.removeEventListener('magicdab-close', onClose)
      window.removeEventListener('magicdab-toggle-play', onToggle)
    }
  }, [])

  // the tray menu shows Play or Stop; the state is resumed at the next start
  useEffect(() => {
    if (!desktop) return
    toHost({ type: 'playing', value: audioEnabled })
    if (resumed.current) {
      try {
        localStorage.setItem('magicdab.desktopPlaying', String(audioEnabled))
      } catch {
        /* storage unavailable */
      }
    }
  }, [audioEnabled])
  useEffect(() => {
    if (!desktop || !ready || resumed.current) return
    resumed.current = true
    let playing = false
    try {
      playing = localStorage.getItem('magicdab.desktopPlaying') === 'true'
    } catch {
      /* storage unavailable */
    }
    if (playing && !useStore.getState().audioEnabled) startAudio()
  }, [ready])

  if (!desktop || !open) return null
  const choose = (action: 'hide' | 'quit' | 'server' | 'cancel') => {
    setOpen(false)
    if (action === 'cancel') {
      toHost({ type: 'close', action })
      return
    }
    if (remember) closePrefs.action = action
    act(action)
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
        {/* (each choice says what goes on and what ends: hidden, the program
            kept running and a second one started beside it could not get the tuner) */}
        <div className="close-options">
          {(['quit', 'hide', 'server'] as const).map((a) => (
            <button key={a} type="button" className={'close-opt' + (a === 'quit' ? ' primary' : '')} autoFocus={a === 'quit'} onClick={() => choose(a)}>
              <b>{t(`close_${a}`)}</b>
              <span>{t(`close_${a}_hint`)}</span>
            </button>
          ))}
        </div>
        <div className="close-actions">
          <label className="check">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} /> {t('close_remember')}
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
