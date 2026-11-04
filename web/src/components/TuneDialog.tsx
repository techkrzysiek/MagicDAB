import { useEffect, useRef, useState } from 'react'
import { Eye, EyeOff, LockKeyhole, LockOpen, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { tuneLogin, tuneLogout } from '../actions'

/** The tuning password: a trusted listener may tune while the tuner is locked. */
export function TuneDialog() {
  const t = useT()
  const open = useStore((s) => s.tuneOpen)
  const tuner = useStore((s) => s.tuner)
  const error = useStore((s) => s.tuneError)
  const set = useStore((s) => s.set)
  const [pw, setPw] = useState('')
  const [show, setShow] = useState(false)
  const [busy, setBusy] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (open) {
      setPw('')
      setBusy(false)
      set({ tuneError: false })
      setTimeout(() => input.current?.focus(), 50)
    }
  }, [open, set])
  useEffect(() => {
    // closed once tuning is unlocked
    if (open && tuner) set({ tuneOpen: false })
  }, [open, tuner, set])
  useEffect(() => {
    if (error) setBusy(false)
  }, [error])
  if (!open) return null
  const close = () => set({ tuneOpen: false })
  const submit = () => {
    if (!pw) return
    setBusy(true)
    set({ tuneError: false })
    tuneLogin(pw)
  }
  return (
    <div className="modal-back" onClick={close}>
      <form
        className="modal login-modal"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        <div className="modal-head">
          <LockOpen size={18} />
          <b>{t('tune_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={close} aria-label="close">
            <X />
          </button>
        </div>
        <p className="hint">{t('tune_help')}</p>
        <div className={'pw-field' + (error ? ' invalid' : '')}>
          <input
            ref={input}
            type={show ? 'text' : 'password'}
            autoComplete="off"
            placeholder={t('tune_password')}
            value={pw}
            onChange={(e) => {
              setPw(e.target.value)
              if (error) set({ tuneError: false })
            }}
          />
          <button type="button" className="icon-btn" onClick={() => setShow(!show)} aria-label={show ? 'hide' : 'show'}>
            {show ? <EyeOff size={18} /> : <Eye size={18} />}
          </button>
        </div>
        {error && <div className="login-error">{t('tune_failed')}</div>}
        <div className="row login-actions">
          <button type="button" className="btn" onClick={close}>
            {t('cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={!pw || busy}>
            {busy ? '…' : t('tune_unlock')}
          </button>
        </div>
      </form>
    </div>
  )
}

/** Next to the channel: the tuner is locked (open the dialog) or unlocked by the password (lock it again). */
export function TuneLockButton() {
  const t = useT()
  const locked = useStore((s) => !!s.hello?.tunerLocked && !s.admin && !s.tuner)
  const offered = useStore((s) => !!s.hello?.tunePassword)
  const tuner = useStore((s) => s.tuner && !s.admin && !!s.hello?.tunerLocked)
  const set = useStore((s) => s.set)
  if (locked && offered) {
    return (
      <button className="icon-btn tune-lock" onClick={() => set({ tuneOpen: true })} title={t('tune_locked_hint')} aria-label={t('tune_title')}>
        <LockKeyhole size={18} />
      </button>
    )
  }
  if (tuner) {
    return (
      <button className="icon-btn tune-lock open" onClick={() => tuneLogout()} title={t('tune_unlocked')} aria-label={t('tune_unlocked')}>
        <LockOpen size={18} />
      </button>
    )
  }
  return null
}
