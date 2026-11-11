import { useEffect, type RefObject } from 'react'
import { Heart } from 'lucide-react'
import { useStore } from '../store'
import { initials, serviceColor, whenShort } from '../util'
import { useT } from '../i18n'
import type { Service } from '../types'
import { useLogoUrl } from './logo'

/**
 * Close an open menu on a click outside the given elements or on Escape (not
 * when the pointer merely leaves it: menus open below or beside their button,
 * and the way there must not close them).
 */
export function useDismiss(refs: RefObject<HTMLElement | null>[], open: boolean, close: () => void) {
  useEffect(() => {
    if (!open) return
    const down = (e: PointerEvent) => {
      if (!refs.some((r) => r.current?.contains(e.target as Node))) close()
    }
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close()
    }
    document.addEventListener('pointerdown', down)
    document.addEventListener('keydown', key)
    return () => {
      document.removeEventListener('pointerdown', down)
      document.removeEventListener('keydown', key)
    }
  }, [open])
}

/** Scroll the list that contains `el` (not the page) so that `el` is visible. */
export function scrollIntoList(el: HTMLElement) {
  let box = el.parentElement
  while (box && !/(auto|scroll)/.test(getComputedStyle(box).overflowY)) box = box.parentElement
  if (!box || box === document.body || box === document.documentElement) return
  const r = el.getBoundingClientRect()
  const b = box.getBoundingClientRect()
  if (r.top < b.top) box.scrollBy({ top: r.top - b.top - 8, behavior: 'smooth' })
  else if (r.bottom > b.bottom) box.scrollBy({ top: r.bottom - b.bottom + 8, behavior: 'smooth' })
}

/** `ecc`: for a station of another multiplex (by default that of the multiplex received now). */
export function Logo({ label, sid, ecc, small, onClick }: { label: string; sid: number; ecc?: number | null; small?: boolean; onClick?: () => void }) {
  const url = useLogoUrl(sid, small ? 36 : 40, ecc)
  return (
    <div
      className={'logo' + (small ? ' small' : '') + (url ? ' has-img' : '')}
      style={{ background: url ? 'transparent' : serviceColor(sid), cursor: onClick ? 'pointer' : undefined }}
      onClick={onClick}
    >
      {url ? <img src={url} alt="" /> : initials(label)}
    </div>
  )
}

export function Flag({ code, size = 18 }: { code?: string | null; size?: number }) {
  if (!code) return null
  return <span className={`fi fi-${code.toLowerCase()}`} style={{ width: size * 1.33, height: size, display: 'inline-block' }} />
}

export function FavStar({ sid }: { sid: number }) {
  const on = useStore((s) => s.favorites.includes(sid))
  const toggle = useStore((s) => s.toggleFavorite)
  return (
    <button
      className={'star' + (on ? ' on' : '')}
      onClick={(e) => {
        e.stopPropagation()
        toggle(sid)
      }}
      title="★"
    >
      <Heart />
    </button>
  )
}

/** Four signal bars coloured by the reception quality. */
export function SignalBars({ level, synced }: { level: number; synced: boolean }) {
  const colors = ['var(--sig-none)', 'var(--sig-low)', 'var(--sig-mid)', 'var(--sig-good)']
  const c = colors[level] ?? colors[0]
  const lit = synced ? level + 1 : level > 0 ? 1 : 0
  return (
    <span className="sigbars">
      {[0, 1, 2, 3].map((i) => (
        <span key={i} style={{ background: i < lit ? c : undefined }} />
      ))}
    </span>
  )
}

/** Header used by Info / TII / Signal / Scanner views. */
export function ViewHeader() {
  const t = useT()
  const ensemble = useStore((s) => s.ensemble)
  const selected = useStore((s) => s.selected)
  const dls = useStore((s) => (s.selected ? s.services[s.selected.sid]?.dls : undefined))
  const svc: Service | undefined = ensemble.services?.find((x) => x.sid === selected?.sid)
  return (
    <div className="view-header">
      {svc && <Logo label={svc.label} sid={svc.sid} small />}
      <div className="titles">
        <div className="title">{svc ? svc.label : t('no_service')}</div>
        <div className="subtitle">{dls?.text ?? ''}</div>
      </div>
      <div className="ens">
        <div>{ensemble.label ?? ''}</div>
        <div>{ensemble.channel ?? ''}</div>
      </div>
    </div>
  )
}

/** Badge for data shown from the FIG cache (not yet received live). */
export function CacheTag({ time }: { time?: number }) {
  const t = useT()
  return (
    <span className="chip cache-tag" title={time ? `${t('from_cache')}: ${new Date(time * 1000).toLocaleString()}` : t('from_cache')}>
      cache{time ? `, ${whenShort(time)}` : ''}
    </span>
  )
}

/** Minimal markdown of server descriptions: [links](url), **bold**, line breaks. */
export function Markdown({ text }: { text: string }) {
  const parts: (string | JSX.Element)[] = []
  const re = /\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)|\*\*([^*]+)\*\*|(https?:\/\/[^\s)]+)|\n/g
  let last = 0
  let m: RegExpExecArray | null
  let k = 0
  while ((m = re.exec(text))) {
    if (m.index > last) parts.push(text.slice(last, m.index))
    if (m[1])
      parts.push(
        <a key={k++} href={m[2]} target="_blank" rel="noreferrer">
          {/* **bold** inside the text of a link */}
          {m[1].split(/\*\*([^*]+)\*\*/).map((x, i) => (i % 2 ? <b key={i}>{x}</b> : x))}
        </a>,
      )
    else if (m[3]) parts.push(<b key={k++}>{m[3]}</b>)
    else if (m[4]) parts.push(<a key={k++} href={m[4]} target="_blank" rel="noreferrer">{m[4]}</a>)
    else parts.push(<br key={k++} />)
    last = m.index + m[0].length
  }
  if (last < text.length) parts.push(text.slice(last))
  return <>{parts}</>
}

/** Links from broadcasts, RadioVIS and SPI documents: web, mail and phone only
 *  (React 18 still follows javascript: links). */
export function safeUrl(u?: string | null): string | undefined {
  const s = u?.trim()
  return s && /^(https?:\/\/|mailto:|tel:)/i.test(s) ? s : undefined
}

/** Text for Leaflet tooltips and markers, which take HTML. */
export const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)

/** The logo (web/public/favicon.svg): the flat-topped spectrum of a DAB
 * ensemble and a sparkle. */
export function MagicDabLogo() {
  return (
    <svg viewBox="0 0 64 64" aria-hidden="true">
      <defs>
        <linearGradient id="magicdab-logo" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#7c4dff" />
          <stop offset="1" stopColor="#5b2fc4" />
        </linearGradient>
      </defs>
      <rect width="64" height="64" rx="14" fill="url(#magicdab-logo)" />
      <path d="M9 47H14L18 27H46L50 47H55" fill="none" stroke="#fff" strokeWidth="4.5" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M14 47L18 27H46L50 47Z" fill="#fff" fillOpacity="0.22" />
      <path d="M47 6Q47 15 56 15Q47 15 47 24Q47 15 38 15Q47 15 47 6Z" fill="#ffd54f" />
    </svg>
  )
}
