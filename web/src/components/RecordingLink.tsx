import { useState } from 'react'
import { Play, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { send } from '../ws'
import { startAudio } from '../actions'

// (taken off the address at once: a reload does not offer it again)
function fromAddress(): string | null {
  try {
    const u = new URL(location.href)
    const name = u.searchParams.get('dxrec')
    if (name) {
      u.searchParams.delete('dxrec')
      history.replaceState(null, '', u.pathname + u.search + u.hash)
    }
    return name
  } catch {
    return null
  }
}
const linked = fromAddress()

/**
 * A link to a DX recording (?dxrec=name, in the report of a new multiplex
 * on Discord): one click plays it on a receiver of this listener's own (the
 * browser lets the sound start only in a click).
 */
export function RecordingLink() {
  const t = useT()
  const [name, setName] = useState(linked)
  const loginOpen = useStore((s) => s.loginOpen)
  const audioOn = useStore((s) => s.audioEnabled)
  if (!name || loginOpen) return null
  const play = () => {
    if (!audioOn) startAudio()
    send({ type: 'dxrec', action: 'play', name })
    setName(null)
  }
  return (
    <div className="modal-back" onClick={() => setName(null)}>
      <div className="modal login-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <Play size={18} />
          <b>{t('dxrec_link_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={() => setName(null)} aria-label="close">
            <X />
          </button>
        </div>
        <p className="hint">{name}</p>
        <button className="btn primary" onClick={play}>
          <Play size={16} /> {t('dxrec_play')}
        </button>
      </div>
    </div>
  )
}
