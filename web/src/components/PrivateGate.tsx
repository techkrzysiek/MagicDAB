import { useEffect, useRef, useState } from 'react'
import { Eye, EyeOff, LockKeyhole, WandSparkles } from 'lucide-react'
import { useStore, sessionGet } from '../store'
import { useT } from '../i18n'
import { enter, stopAudio } from '../actions'

/**
 * The gate of a private server: the whole page until the administrator or the
 * tuning password is given (the server sends nothing else before). Shown also
 * when the server turned private or the listener logged out.
 */
export function PrivateGate() {
  const t = useT()
  const name = useStore((s) => s.hello?.stationName)
  const version = useStore((s) => s.hello?.version)
  const sourceUrl = useStore((s) => s.hello?.sourceUrl)
  const tunePassword = useStore((s) => !!s.hello?.tunePassword)
  const connection = useStore((s) => s.connection)
  const gate = useStore((s) => s.gate)
  const audioEnabled = useStore((s) => s.audioEnabled)
  const [pw, setPw] = useState('')
  const [show, setShow] = useState(false)
  // a stored password is tried by itself after a connect: a moment before the form
  const [waiting, setWaiting] = useState(() => !!(sessionGet('magicdab.pw') || sessionGet('magicdab.tunepw')))
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (!waiting) return
    const id = setTimeout(() => setWaiting(false), 1500)
    return () => clearTimeout(id)
  }, [waiting])
  useEffect(() => {
    if (!waiting) input.current?.focus()
  }, [waiting])
  // (the sound of what was heard before stops here, and the background picture goes)
  useEffect(() => {
    if (audioEnabled) stopAudio()
  }, [audioEnabled])
  useEffect(() => {
    delete document.documentElement.dataset.bg
  }, [])
  const busy = gate.pending !== null
  const submit = () => {
    if (!pw || busy) return
    enter(pw)
  }
  const named = !!name && name !== 'MagicDAB'
  return (
    <div className="gate">
      <form
        className="gate-card"
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        <div className="gate-brand">
          <WandSparkles />
          <span>MagicDAB</span>
        </div>
        {named && <h1 className="gate-name">{name}</h1>}
        <div className="gate-lock">
          <LockKeyhole size={28} />
          <b>{t('gate_title')}</b>
        </div>
        {waiting ? (
          <p className="hint">…</p>
        ) : (
          <>
            <p className="hint">{t(tunePassword ? 'gate_help' : 'gate_help_admin')}</p>
            <div className={'pw-field' + (gate.error ? ' invalid' : '')}>
              <input
                ref={input}
                type={show ? 'text' : 'password'}
                autoComplete="current-password"
                placeholder={t('password')}
                value={pw}
                onChange={(e) => setPw(e.target.value)}
              />
              <button type="button" className="icon-btn" onClick={() => setShow(!show)} aria-label={show ? 'hide' : 'show'}>
                {show ? <EyeOff size={18} /> : <Eye size={18} />}
              </button>
            </div>
            {gate.error && <div className="login-error">{t('login_failed')}</div>}
            {gate.locked && <div className="login-error">{t('login_locked')}</div>}
            {connection !== 'open' && <div className="hint">{t('connection_lost')}</div>}
            <button type="submit" className="btn primary gate-enter" disabled={!pw || busy || connection !== 'open'}>
              {busy ? '…' : t('gate_enter')}
            </button>
          </>
        )}
        <div className="gate-foot hint">
          MagicDAB {version}
          {sourceUrl && (
            <>
              {', '}
              <a href={sourceUrl} target="_blank" rel="noreferrer">
                {t('source_code')}
              </a>
            </>
          )}
        </div>
      </form>
    </div>
  )
}
