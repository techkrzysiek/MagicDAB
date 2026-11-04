import { useEffect } from 'react'
import { Keyboard, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { stepChannel, stepService, stepVolume, toggleAudio, toggleMute } from '../actions'

/** The keys and what they do, as listed in the help (`?`). */
const LIST: { keys: string[][]; what: string }[] = [
  { keys: [['Space']], what: 'kb_play' },
  { keys: [['←'], ['→']], what: 'kb_station' },
  { keys: [['Shift', '←'], ['Shift', '→']], what: 'kb_channel' },
  { keys: [['+'], ['−']], what: 'kb_volume' },
  { keys: [['M']], what: 'kb_mute' },
  { keys: [['?']], what: 'kb_help' },
]

/** All keys go to a field being typed in (and to a select). */
function typing(el: EventTarget | null) {
  if (!(el instanceof HTMLElement)) return false
  if (el.isContentEditable || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') return true
  return el instanceof HTMLInputElement && !['range', 'checkbox', 'radio', 'button', 'submit', 'reset', 'color', 'file'].includes(el.type)
}

function onKey(e: KeyboardEvent) {
  if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || e.isComposing || typing(e.target)) return
  const st = useStore.getState()
  if (st.shortcutsOpen) {
    if (e.key === 'Escape' || e.key === '?') {
      e.preventDefault()
      st.set({ shortcutsOpen: false })
    }
    return
  }
  // a dialog keeps its keys (login, the wizard, closing the window)
  if (document.querySelector('.modal-back')) return
  // buttons, links and checkboxes take Space themselves, sliders and radio buttons the arrows
  const el = e.target instanceof Element ? e.target : null
  const input = el instanceof HTMLInputElement ? el.type : null
  const ownsSpace = !!el?.closest('button, a, summary, [role="button"]') || (input !== null && input !== 'range')
  const ownsArrows = input === 'range' || input === 'radio'
  // (held down: the volume only; stations and channels one step per press)
  const step = (f: () => void) => {
    if (!e.repeat) f()
  }
  switch (e.key) {
    case ' ':
      if (ownsSpace) return
      step(toggleAudio)
      break
    case 'm':
    case 'M':
      step(toggleMute)
      break
    case '+':
    case '=':
      stepVolume(0.05)
      break
    case '-':
    case '_':
      stepVolume(-0.05)
      break
    case 'ArrowLeft':
    case 'ArrowRight': {
      if (ownsArrows) return
      const dir = e.key === 'ArrowLeft' ? -1 : 1
      // (the tuner lock and a fixed channel are checked by stepChannel)
      step(() => (e.shiftKey ? stepChannel(dir) : stepService(dir)))
      break
    }
    case '?':
      st.set({ shortcutsOpen: true })
      break
    default:
      return
  }
  e.preventDefault()
}

/** Keyboard shortcuts for listening and tuning, and their list (`?`). */
export function Shortcuts() {
  const t = useT()
  const open = useStore((s) => s.shortcutsOpen)
  const fixed = useStore((s) => !!(s.state?.fixedChannel ?? s.hello?.fixedChannel))
  const locked = useStore((s) => !!s.hello?.tunerLocked && !s.admin)
  const set = useStore((s) => s.set)
  useEffect(() => {
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  if (!open) return null
  return (
    <div className="modal-back" onClick={() => set({ shortcutsOpen: false })}>
      <div className="modal shortcuts-modal" role="dialog" aria-label={t('kb_title')} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <Keyboard size={18} />
          <b>{t('kb_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={() => set({ shortcutsOpen: false })} aria-label="close">
            <X />
          </button>
        </div>
        <table className="shortcuts">
          <tbody>
            {LIST.map((s) => (
              <tr key={s.what}>
                <td className="keys">
                  {s.keys.map((combo, i) => (
                    <span key={i} className="combo">
                      {combo.map((k) => (
                        <kbd key={k}>{k === 'Space' ? t('kb_space') : k}</kbd>
                      ))}
                    </span>
                  ))}
                </td>
                <td>
                  {t(s.what)}
                  {s.what === 'kb_channel' && (fixed || locked) && <div className="hint">{t(fixed ? 'channel_fixed' : 'kb_locked')}</div>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="hint">{t('kb_hint')}</p>
      </div>
    </div>
  )
}
