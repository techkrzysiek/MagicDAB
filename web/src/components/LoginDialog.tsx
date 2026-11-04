import { useEffect, useRef, useState } from 'react'
import { Eye, EyeOff, KeyRound, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { login } from '../actions'

/** Administrator login (replaces the browser prompt). */
export function LoginDialog() {
  const t = useT()
  const open = useStore((s) => s.loginOpen)
  const admin = useStore((s) => s.admin)
  const error = useStore((s) => s.loginError)
  const set = useStore((s) => s.set)
  const [pw, setPw] = useState('')
  const [show, setShow] = useState(false)
  const [busy, setBusy] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (open) {
      setPw('')
      setBusy(false)
      set({ loginError: false })
      setTimeout(() => input.current?.focus(), 50)
    }
  }, [open, set])
  useEffect(() => {
    // close after a successful login
    if (open && admin) set({ loginOpen: false })
  }, [open, admin, set])
  useEffect(() => {
    if (error) setBusy(false)
  }, [error])
  if (!open) return null
  const submit = () => {
    if (!pw) return
    setBusy(true)
    set({ loginError: false })
    login(pw)
  }
  return (
    <div className="modal-back" onClick={() => set({ loginOpen: false })}>
      <form
        className="modal login-modal"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        <div className="modal-head">
          <KeyRound size={18} />
          <b>{t('login_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={() => set({ loginOpen: false })} aria-label="close">
            <X />
          </button>
        </div>
        <p className="hint">{t('login_help')}</p>
        <div className={'pw-field' + (error ? ' invalid' : '')}>
          <input
            ref={input}
            type={show ? 'text' : 'password'}
            autoComplete="current-password"
            placeholder={t('password')}
            value={pw}
            onChange={(e) => {
              setPw(e.target.value)
              if (error) set({ loginError: false })
            }}
          />
          <button type="button" className="icon-btn" onClick={() => setShow(!show)} aria-label={show ? 'hide' : 'show'}>
            {show ? <EyeOff size={18} /> : <Eye size={18} />}
          </button>
        </div>
        {error && <div className="login-error">{t('login_failed')}</div>}
        <div className="row login-actions">
          <button type="button" className="btn" onClick={() => set({ loginOpen: false })}>
            {t('cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={!pw || busy}>
            {busy ? '…' : t('login')}
          </button>
        </div>
      </form>
    </div>
  )
}
