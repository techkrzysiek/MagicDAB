import { useEffect, useRef, useState } from 'react'
import { ChevronLeft, ChevronRight, Newspaper, X } from 'lucide-react'
import { useDismiss } from './common'
import { useStore } from '../store'
import { useT } from '../i18n'
import { setSubscriptions } from '../ws'
import { findService } from '../util'
import type { JmlObject, Service } from '../types'

/** Journaline of a service: in its own packet mode component (a secondary one,
 *  SCIdS 1...) or in the X-PAD of its audio (SCIdS 0); UA type 0x44A. */
export function journalineSources(svc: Service): { scids: number; label: string }[] {
  return svc.components
    .filter((c) => c.userApps.some((u) => u.type === 0x44a))
    .map((c) => ({ scids: c.tmid === 3 ? (c.scids ?? 0) : 0, label: c.label?.trim() || svc.label }))
}

/** One page: its title, links to other pages, text, rows of a list. */
function Page({ obj, onOpen }: { obj: JmlObject; onOpen: (id: number) => void }) {
  return (
    <div className="jml-page">
      {obj.title && <div className="jml-title">{obj.title}</div>}
      {obj.items.map((it, i) =>
        it.type === 'link' ? (
          <button key={i} className="jml-link" onClick={() => onOpen(it.target)}>
            <span>{it.text}</span>
            <ChevronRight size={16} />
          </button>
        ) : it.type === 'text' ? (
          it.text.split('\n').map((p, j) => (p.trim() ? <p key={`${i}.${j}`}>{p}</p> : null))
        ) : (
          <div key={i} className="jml-row">
            {it.cells.map((c, j) => (
              <span key={j}>{c}</span>
            ))}
          </div>
        ),
      )}
    </div>
  )
}

/** Pages watching Journaline (the button, the dialog): the server sends it while one does. */
let watching = 0
function useJournalineWatch(on: boolean) {
  useEffect(() => {
    if (!on) return
    if (watching++ === 0) setSubscriptions({ journaline: true })
    return () => {
      if (--watching === 0) setSubscriptions({ journaline: false })
    }
  }, [on])
}

/** Opens the Journaline of a service's component in the dialog (JournalineDialog). */
export function openJournaline(sid: number, scids: number, label: string) {
  useStore.getState().set({ journalineOpen: { sid, scids, label } })
}

/** The Journaline of the station playing: a button in the station panel (the
 *  pages in the dialog). */
export function JournalineCard({ svc }: { svc: Service }) {
  const t = useT()
  const base = useStore((s) => findService(s.ensemble, svc.sid)) ?? svc
  const src = journalineSources(base)[0]
  const key = src ? `${svc.sid}:${src.scids}` : ''
  const pages = useStore((s) => (key ? Object.keys(s.journaline[key] ?? {}).length : 0))
  // (from the button on, so the pages are there when the dialog opens)
  useJournalineWatch(!!src)
  if (!src) return null
  return (
    <button className="jml-open" onClick={() => openJournaline(svc.sid, src.scids, src.label)} title="Journaline">
      <Newspaper size={18} />
      <span>
        <b>{src.label}</b>
        <span className="hint">Journaline{pages ? `, ${t('jml_pages').replace('{n}', String(pages))}` : ''}</span>
      </span>
      <ChevronRight size={16} />
    </button>
  )
}

/** The dialog with the pages of the Journaline opened (from the station panel
 *  or the list of the multiplex), browsed like on a radio: menus, articles, back. */
export function JournalineDialog() {
  const t = useT()
  const open = useStore((s) => s.journalineOpen)
  const set = useStore((s) => s.set)
  const [stack, setStack] = useState<number[]>([0])
  const box = useRef<HTMLDivElement>(null)
  const key = open ? `${open.sid}:${open.scids}` : ''
  const objs = useStore((s) => (key ? s.journaline[key] : undefined))
  const close = () => set({ journalineOpen: null })
  useDismiss([box], !!open, close)
  useJournalineWatch(!!open)
  // another one opened: from its first page
  useEffect(() => setStack([0]), [key])
  if (!open) return null
  const obj = objs?.[stack[stack.length - 1]]
  return (
    <div className="modal-back">
      <div className="modal jml-modal" ref={box} role="dialog" aria-label={open.label}>
        <div className="modal-head">
          <Newspaper size={18} className="jml-icon" />
          <b>{open.label}</b>
          <span className="hint">Journaline</span>
          <span style={{ flex: 1 }} />
          {stack.length > 1 && (
            <button className="btn" onClick={() => setStack(stack.slice(0, -1))}>
              <ChevronLeft size={16} /> {t('jml_back')}
            </button>
          )}
          <button className="icon-btn" onClick={close} aria-label={t('close')}>
            <X />
          </button>
        </div>
        <div className="jml-body">
          {obj ? <Page obj={obj} onOpen={(next) => setStack([...stack, next])} /> : <div className="hint">{t('jml_waiting')}</div>}
        </div>
      </div>
    </div>
  )
}

/** Every Journaline of the multiplex (Multipleks view): a row each, opened in the dialog. */
export function JournalineList() {
  const t = useT()
  const services = useStore((s) => s.ensemble.services)
  const counts = useStore((s) => s.journaline)
  const all = (services ?? []).flatMap((svc) => journalineSources(svc).map((j) => ({ svc, ...j })))
  useJournalineWatch(all.length > 0)
  if (!all.length) return null
  return (
    <div className="card jml-list">
      <div className="card-title">Journaline</div>
      {all.map(({ svc, scids, label }) => {
        const pages = Object.keys(counts[`${svc.sid}:${scids}`] ?? {}).length
        return (
          <button key={`${svc.sid}:${scids}`} className="jml-open" onClick={() => openJournaline(svc.sid, scids, label)}>
            <Newspaper size={18} />
            <span>
              <b>{label}</b>
              <span className="hint">
                {t('jml_in').replace('{s}', svc.label)}
                {pages ? `, ${t('jml_pages').replace('{n}', String(pages))}` : ''}
              </span>
            </span>
            <ChevronRight size={16} />
          </button>
        )
      })}
    </div>
  )
}
