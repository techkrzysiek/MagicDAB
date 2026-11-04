// Building blocks of the settings: cards of rows, switches, and the draft of
// server settings that one bar saves (or discards) for all tabs at once.
import { useEffect, type ReactNode } from 'react'
import { create } from 'zustand'
import { useStore } from '../store'
import { useT } from '../i18n'
import { saveSettings } from '../actions'

/** A section of a settings tab. */
export function Card({ title, badge, note, children }: { title: string; badge?: string; note?: ReactNode; children: ReactNode }) {
  return (
    <section className="set-card">
      <h3>
        {title}
        {badge && <span className="set-badge">{badge}</span>}
      </h3>
      {note && <div className="set-note">{note}</div>}
      <div className="set-rows">{children}</div>
    </section>
  )
}

/** A setting: what it is (and a hint) on the left, its control on the right;
 * wide: the control below, over the whole width (lists, texts). */
export function Row({ label, hint, wide, children }: { label: ReactNode; hint?: ReactNode; wide?: boolean; children?: ReactNode }) {
  return (
    <div className={'set-row' + (wide ? ' wide' : '')}>
      <div className="set-label">
        <div className="set-name">{label}</div>
        {hint && <div className="hint">{hint}</div>}
      </div>
      {children !== undefined && <div className="set-control">{children}</div>}
    </div>
  )
}

export function Toggle({ checked, onChange, disabled, label }: { checked: boolean; onChange: (v: boolean) => void; disabled?: boolean; label?: string }) {
  return (
    <label className={'toggle' + (disabled ? ' disabled' : '')}>
      <input type="checkbox" role="switch" aria-label={label} checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <span />
    </label>
  )
}

/** One of a few choices as buttons side by side. */
export function Choice<T extends string>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="seg set-seg">
      {options.map(([v, label]) => (
        <button key={v} className={v === value ? 'on' : ''} onClick={() => onChange(v)}>
          {label}
        </button>
      ))}
    </div>
  )
}

// ------------------------------------------------------------------ draft

/** How a field of the draft is checked and sent (the draft keeps what the
 * controls show, e.g. the text of the location). */
interface Rule {
  ok?: (v: any) => boolean
  encode?: (v: any) => unknown
}

const rules: Record<string, Rule> = {}

/** Fields of the settings command whose draft values need checking or converting. */
export function rule(key: string, r: Rule) {
  rules[key] = r
}

interface Draft {
  /** changed fields, by their names in the settings command */
  values: Record<string, unknown>
  put: (key: string, value: unknown, current: unknown) => void
  drop: (key: string) => void
  clear: () => void
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)

/** (outside the view: changes survive a look at another view) */
export const useDraft = create<Draft>((set, get) => ({
  values: {},
  put: (key, value, current) => {
    const values = { ...get().values }
    if (same(value, current)) delete values[key]
    else values[key] = value
    set({ values })
  },
  drop: (key) => {
    const values = { ...get().values }
    delete values[key]
    set({ values })
  },
  clear: () => set({ values: {} }),
}))

/** A server setting as edited: the draft's value, else the saved one. */
export function useField<T>(key: string, current: T): [T, (v: T) => void, boolean] {
  const draft = useDraft((d) => d.values[key])
  const changed = draft !== undefined
  const value = changed ? (draft as T) : current
  return [value, (v: T) => useDraft.getState().put(key, v, current), changed]
}

export const fieldOk = (key: string, v: unknown) => rules[key]?.ok?.(v) ?? true

/** The bar under the settings while server settings are changed and not saved. */
/** where: the tab of a field (named when a field to correct is elsewhere) */
export function SaveBar({ where }: { where?: (key: string) => string }) {
  const t = useT()
  const values = useDraft((d) => d.values)
  const admin = useStore((s) => s.admin)
  const keys = Object.keys(values)
  // (a page left with changes asks first)
  useEffect(() => {
    if (!keys.length) return
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault()
      // (older browsers ask only with this)
      e.returnValue = ''
    }
    window.addEventListener('beforeunload', warn)
    return () => window.removeEventListener('beforeunload', warn)
  }, [keys.length])
  if (!keys.length || !admin) return null
  const bad = keys.filter((k) => !fieldOk(k, values[k]))
  const save = () => {
    const out: Record<string, unknown> = {}
    for (const k of keys) out[k] = rules[k]?.encode ? rules[k].encode!(values[k]) : values[k]
    saveSettings(out)
    // (reconnecting logs in again with it)
    if (typeof out.adminPassword === 'string' && out.adminPassword) sessionStorage.setItem('magicdab.pw', out.adminPassword)
    useDraft.getState().clear()
  }
  return (
    <div className="save-bar" role="region" aria-label={t('set_unsaved').replace('{n}', String(keys.length))}>
      <span className="save-bar-text">
        {bad.length ? t('set_fix_fields') + (where ? `: ${[...new Set(bad.map(where))].join(', ')}` : '') : t('set_unsaved').replace('{n}', String(keys.length))}
      </span>
      <button className="btn" onClick={() => useDraft.getState().clear()}>
        {t('set_discard')}
      </button>
      <button className="btn primary" disabled={bad.length > 0} onClick={save}>
        {t('save')}
      </button>
    </div>
  )
}
