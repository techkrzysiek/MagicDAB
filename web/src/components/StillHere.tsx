import { useEffect, useState } from 'react'
import { Headphones } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { stillHere, stopAudio } from '../actions'

/**
 * "Are you still listening?" (Settings > Access): the server asks after a
 * while without an action of the listener and closes the connection without
 * an answer in time. The dialog counts down; any click on it answers.
 */
export function StillHere() {
  const t = useT()
  const asked = useStore((s) => s.stillHere)
  const closedBy = useStore((s) => s.closedBy)
  const audioEnabled = useStore((s) => s.audioEnabled)
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!asked) return
    setNow(Date.now())
    const i = setInterval(() => setNow(Date.now()), 500)
    return () => clearInterval(i)
  }, [asked])
  // shown out: the sound stops (the banner says why, a click connects again)
  useEffect(() => {
    if (closedBy === 'idle' && audioEnabled) stopAudio()
  }, [closedBy, audioEnabled])
  if (!asked) return null
  const left = Math.max(0, Math.ceil(asked.seconds - (now - asked.at) / 1000))
  return (
    <div className="modal-back" onClick={stillHere}>
      <div className="modal login-modal still-here" role="alertdialog" aria-label={t('still_title')} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <Headphones size={18} />
          <b>{t('still_title')}</b>
        </div>
        <p className="hint">{t('still_text').replace('{s}', String(left))}</p>
        <div className="still-count" aria-hidden>
          {left}
        </div>
        <div className="row login-actions">
          <button type="button" className="btn primary" autoFocus onClick={stillHere}>
            {t('still_yes')}
          </button>
        </div>
      </div>
    </div>
  )
}
