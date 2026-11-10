import { createContext, useContext, useEffect, useMemo, useState } from 'react'
import { ArrowLeft, FileCode2, Image as ImageIcon, Link2, Radio, Tag } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { requestLogo, send } from '../ws'
import { whenShort } from '../util'
import { safeUrl } from '../components/common'
import { genreLabel, genrePath } from '../genres'
import type { SpiDoc, SpiIssue } from '../types'

const OpenPicture = createContext<(p: Picture) => void>(() => undefined)

/** Child elements with a local name (namespaces ignored). */
function kids(el: Element | null | undefined, name?: string): Element[] {
  if (!el) return []
  return Array.from(el.children).filter((c) => !name || c.localName === name)
}

function textOf(el: Element | null | undefined) {
  return el?.textContent?.trim() ?? ''
}

function fmtSize(n: number) {
  return n >= 1024 * 1024 ? `${(n / 1048576).toFixed(1)} MB` : n >= 1024 ? `${(n / 1024).toFixed(1)} kB` : `${n} B`
}

interface ParsedBearer {
  kind: string
  id: string
  cost?: string
  offset?: string
  bitrate?: string
  mime?: string
  pi?: string
  khz?: number
  eid?: number
  sid?: string
  cc?: string
  scids?: string
}

/** dab:<gcc>.<eid>.<sid>.<scids>, fm:<gcc>.<pi>.<freq> (country as GCC or ISO code), URLs. */
function parseBearer(el: Element): ParsedBearer {
  const id = el.getAttribute('id') ?? ''
  const b: ParsedBearer = {
    kind: 'IP',
    id,
    cost: el.getAttribute('cost') ?? undefined,
    offset: el.getAttribute('offset') ?? undefined,
    bitrate: el.getAttribute('bitrate') ?? undefined,
    mime: el.getAttribute('mimeValue') ?? undefined,
  }
  const dab = id.match(/^dab:([0-9a-f]{3}|[a-z]{2})\.([0-9a-f]{4})\.([0-9a-f]{4,8})\.([0-9a-f]+)/i)
  const fm = id.match(/^fm:([0-9a-f]{3}|[a-z]{2})\.([0-9a-f]{4})\.(\d+)/i)
  if (dab) {
    Object.assign(b, {
      kind: 'DAB',
      eid: parseInt(dab[2], 16),
      sid: dab[3].toUpperCase(),
      cc: dab[1].length === 3 ? `ECC ${dab[1].slice(1).toUpperCase()}` : dab[1].toUpperCase(),
      scids: dab[4],
    })
  } else if (fm) {
    Object.assign(b, { kind: 'FM', pi: fm[2].toUpperCase(), khz: parseInt(fm[3]) * 10 })
  } else if (/^(drm|amss|hd):/i.test(id)) {
    b.kind = id.split(':')[0].toUpperCase()
  }
  return b
}

/** Short form of a stream URL: host and file name. */
function shortUrl(u: string) {
  try {
    const x = new URL(u)
    const file = x.pathname.split('/').filter(Boolean).pop() ?? ''
    return x.host + (file ? ' / ' + file : '')
  } catch {
    return u
  }
}

/** Bearers of a service, compact: duplicates removed, FM grouped by PI, one line per DAB service and stream. */
function Bearers({ els }: { els: Element[] }) {
  const t = useT()
  const serviceList = useStore((s) => s.serviceList)
  const list = els.map(parseBearer)
  if (!list.length) return null
  const fm = new Map<string, { khz: Set<number>; cost: Set<string> }>()
  const dab = new Map<string, ParsedBearer>()
  const ip = new Map<string, ParsedBearer>()
  const other: ParsedBearer[] = []
  for (const b of list) {
    if (b.kind === 'FM' && b.pi) {
      const g = fm.get(b.pi) ?? { khz: new Set<number>(), cost: new Set<string>() }
      g.khz.add(b.khz!)
      if (b.cost) g.cost.add(b.cost)
      fm.set(b.pi, g)
    } else if (b.kind === 'DAB') {
      const k = `${b.eid}.${b.sid}.${b.scids}`
      if (!dab.has(k)) dab.set(k, b)
    } else if (b.kind === 'IP') {
      if (!ip.has(b.id)) ip.set(b.id, b)
    } else if (!other.some((o) => o.id === b.id)) other.push(b)
  }
  const costs = (c: Set<string>) => (c.size ? `cost ${[...c].join('/')}` : '')
  return (
    <div className="spi-bearers">
      {[...dab.values()].map((b) => {
        const muxes = serviceList.filter((e) => e.eid === b.eid).map((e) => e.channel)
        return (
          <div key={b.id} className="spi-bearer">
            <span className="link-kind">DAB</span>
            <span className="spi-b-main">
              EId {b.eid!.toString(16).toUpperCase().padStart(4, '0')} · SId {b.sid} · {b.cc}
              {b.scids !== '0' ? ` · SCIdS ${b.scids}` : ''}
              {muxes.map((m) => (
                <span key={m} className="mux-chip">
                  {m}
                </span>
              ))}
              <span className="hint">{[b.cost && `cost ${b.cost}`, b.mime].filter(Boolean).join(' · ')}</span>
            </span>
          </div>
        )
      })}
      {[...fm.entries()].map(([pi, g]) => (
        <div key={pi} className="spi-bearer">
          <span className="link-kind">FM</span>
          <span className="spi-b-main">
            <span>
              PI {pi} · {g.khz.size} {t('spi_freqs')}
            </span>
            <span className="hint">{costs(g.cost)}</span>
            <span className="spi-freqs">
              {[...g.khz]
                .sort((a, b) => a - b)
                .map((k) => (
                  <span key={k} className="freq-chip">
                    {(k / 1000).toFixed(2)}
                  </span>
                ))}
            </span>
          </span>
        </div>
      ))}
      {[...ip.values()].map((b) => (
        <div key={b.id} className="spi-bearer">
          <span className="link-kind">IP</span>
          <span className="spi-b-main">
            {safeUrl(b.id) ? (
              <a href={safeUrl(b.id)} target="_blank" rel="noreferrer" title={b.id}>
                {shortUrl(b.id)}
              </a>
            ) : (
              <span title={b.id}>{shortUrl(b.id)}</span>
            )}
            <span className="hint">
              {[b.bitrate && `${b.bitrate} kbps`, b.mime, b.cost && `cost ${b.cost}`, b.offset && `+${b.offset} ms`].filter(Boolean).join(' · ')}
            </span>
          </span>
        </div>
      ))}
      {other.map((b) => (
        <div key={b.id} className="spi-bearer">
          <span className="link-kind">{b.kind}</span>
          <span className="spi-b-main">{b.id}</span>
        </div>
      ))}
    </div>
  )
}

function Names({ el }: { el: Element }) {
  const rows: [string, string][] = []
  for (const n of ['shortName', 'mediumName', 'longName']) {
    for (const e of kids(el, n)) rows.push([n.replace('Name', ''), textOf(e) + (e.getAttribute('xml:lang') ? ` (${e.getAttribute('xml:lang')})` : '')])
  }
  if (!rows.length) return null
  return (
    <div className="spi-names">
      {rows.map(([k, v], i) => (
        <span key={i} className={'spi-name ' + k}>
          <span className="hint">{k}</span> {v}
        </span>
      ))}
    </div>
  )
}

interface Picture {
  src: string
  url: string
  size: string
  mime: string
}

/** Image of a multimedia element from the server's logo cache: RadioDNS logos
 * by URL, MOT objects (SPI over the air) by content name. Nothing is loaded
 * from the internet by the browser. */
function useMediaSrc(url: string): string | null {
  const name = url.replace(/^\//, '')
  const variant = useStore((s) => {
    for (const list of Object.values(s.logos)) {
      const v = list.find((x) => x.url === url || x.url.replace(/^\//, '') === name)
      if (v) return v
    }
    return undefined
  })
  const blob = useStore((s) => (variant ? s.logoUrls[variant.id] : undefined))
  useEffect(() => {
    if (variant && !blob) requestLogo(variant.id)
  }, [variant, blob])
  return blob ?? null
}

function MediaItem({ m, onOpen }: { m: Element; onOpen: (p: Picture) => void }) {
  const t = useT()
  const url = m.getAttribute('url') ?? ''
  const w = m.getAttribute('width')
  const h = m.getAttribute('height')
  const mime = m.getAttribute('mimeValue') ?? ''
  const src = useMediaSrc(url)
  const size = w && h ? `${w}×${h}` : m.getAttribute('type') ?? ''
  return (
    <button className="spi-logo" title={src ? url : `${url} – ${t('not_cached')}`} onClick={() => src && onOpen({ src, url, size, mime })} disabled={!src}>
      {src ? <img src={src} alt="" loading="lazy" /> : <ImageIcon size={18} />}
      <span className="hint">
        {size} {mime.replace('image/', '')}
      </span>
    </button>
  )
}

function Media({ el }: { el: Element }) {
  const open = useContext(OpenPicture)
  const descs: string[] = []
  const media: Element[] = []
  for (const md of kids(el, 'mediaDescription')) {
    for (const d of [...kids(md, 'shortDescription'), ...kids(md, 'longDescription')]) descs.push(textOf(d))
    media.push(...kids(md, 'multimedia'))
  }
  return (
    <>
      {descs.filter(Boolean).map((d, i) => (
        <p key={i} className="spi-desc">
          {d}
        </p>
      ))}
      {media.length > 0 && (
        <div className="spi-media">
          {media.map((m, i) => (
            <MediaItem key={i} m={m} onOpen={open} />
          ))}
        </div>
      )}
    </>
  )
}

function Extras({ el }: { el: Element }) {
  const genres = kids(el, 'genre')
  const links = kids(el, 'link')
  const keywords = kids(el, 'keywords').map(textOf).filter(Boolean)
  const rdns = kids(el, 'radiodns')[0]
  return (
    <>
      {genres.length > 0 && (
        <div className="spi-row">
          <Tag size={14} />
          {genres.map((g, i) => (
            <span key={i} className="chip" title={[genrePath(g.getAttribute('href') ?? ''), g.getAttribute('href')].filter(Boolean).join('\n')}>
              {textOf(g) || genreLabel(g.getAttribute('href') ?? '')}
            </span>
          ))}
        </div>
      )}
      {keywords.length > 0 && <div className="spi-row hint">{keywords.join(', ')}</div>}
      {links.map((l, i) => {
        const uri = l.getAttribute('uri') ?? l.getAttribute('url')
        const href = safeUrl(uri)
        const label = l.getAttribute('description') || uri
        return (
          <div key={i} className="spi-row">
            <Link2 size={14} />
            {href ? (
              <a href={href} target="_blank" rel="noreferrer">
                {label}
              </a>
            ) : (
              <span>{label}</span>
            )}
            {l.getAttribute('mimeValue') && <span className="hint">{l.getAttribute('mimeValue')}</span>}
          </div>
        )
      })}
      {rdns && (
        <div className="spi-row hint">
          RadioDNS: {rdns.getAttribute('fqdn')} · {rdns.getAttribute('serviceIdentifier')}
        </div>
      )}
    </>
  )
}

function ServiceInfo({ root, query }: { root: Element; query: string }) {
  const t = useT()
  const services = Array.from(root.getElementsByTagNameNS('*', 'service'))
  const provider = Array.from(root.getElementsByTagNameNS('*', 'serviceProvider'))[0]
  const q = query.toLowerCase()
  const shown = services.filter((s) => !q || (s.textContent ?? '').toLowerCase().includes(q) || kids(s, 'bearer').some((b) => (b.getAttribute('id') ?? '').toLowerCase().includes(q)))
  const attrs = ['version', 'creationTime', 'originator', 'serviceProvider', 'xml:lang'].map((a) => [a, root.getAttribute(a)]).filter(([, v]) => v)
  return (
    <div>
      {attrs.length > 0 && (
        <div className="spi-head hint">
          {attrs.map(([k, v]) => (
            <span key={k}>
              {k}: <b>{v}</b>
            </span>
          ))}
        </div>
      )}
      {provider && (
        <div className="spi-card">
          <div className="spi-card-title">{t('spi_provider')}</div>
          <Names el={provider} />
          <Media el={provider} />
          <Extras el={provider} />
        </div>
      )}
      <div className="spi-count">
        {t('services')}: {shown.length}
        {shown.length !== services.length ? ` / ${services.length}` : ''}
      </div>
      {shown.map((s, i) => (
        <div key={i} className="spi-card">
          <Names el={s} />
          <Media el={s} />
          <Extras el={s} />
          <Bearers els={kids(s, 'bearer')} />
        </div>
      ))}
    </div>
  )
}

function Programmes({ root, query }: { root: Element; query: string }) {
  const lang = useStore((s) => s.lang)
  const progs = Array.from(root.getElementsByTagNameNS('*', 'programme'))
  const q = query.toLowerCase()
  const rows = progs
    .map((p) => {
      const time = p.getElementsByTagNameNS('*', 'time')[0]
      const start = time?.getAttribute('time') ?? time?.getAttribute('actualTime') ?? ''
      return {
        start,
        dur: time?.getAttribute('duration') ?? '',
        name: textOf(kids(p, 'mediumName')[0] ?? kids(p, 'longName')[0] ?? kids(p, 'shortName')[0]),
        desc: textOf(p.getElementsByTagNameNS('*', 'shortDescription')[0] ?? p.getElementsByTagNameNS('*', 'longDescription')[0]),
        genres: kids(p, 'genre').map((g) => textOf(g) || genreLabel(g.getAttribute('href') ?? '')),
      }
    })
    .filter((r) => !q || `${r.name} ${r.desc}`.toLowerCase().includes(q))
    .sort((a, b) => a.start.localeCompare(b.start))
  return (
    <table className="spi-table">
      <tbody>
        {rows.map((r, i) => (
          <tr key={i}>
            <td className="nowrap">{r.start ? new Date(r.start).toLocaleString(lang, { weekday: 'short', hour: '2-digit', minute: '2-digit' }) : ''}</td>
            <td className="hint nowrap">{r.dur.replace('PT', '').toLowerCase()}</td>
            <td>
              <b>{r.name}</b>
              {r.desc && <div className="hint">{r.desc}</div>}
            </td>
            <td className="hint">{r.genres.join(', ')}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** Indented XML of a parsed document (RadioDNS files are often on one line). */
function prettyXml(doc: Document) {
  const out: string[] = []
  const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  const walk = (el: Element, depth: number) => {
    const pad = '  '.repeat(depth)
    const attrs = Array.from(el.attributes).map((a) => ` ${a.name}="${esc(a.value)}"`).join('')
    const children = Array.from(el.children)
    const text = children.length ? '' : (el.textContent ?? '').trim()
    if (!children.length && !text) out.push(`${pad}<${el.nodeName}${attrs}/>`)
    else if (!children.length) out.push(`${pad}<${el.nodeName}${attrs}>${esc(text)}</${el.nodeName}>`)
    else {
      out.push(`${pad}<${el.nodeName}${attrs}>`)
      children.forEach((c) => walk(c, depth + 1))
      out.push(`${pad}</${el.nodeName}>`)
    }
  }
  if (doc.documentElement) walk(doc.documentElement, 0)
  return out.join('\n')
}

function DocDetail({ doc, onBack }: { doc: SpiDoc; onBack: () => void }) {
  const t = useT()
  const xml = useStore((s) => s.spiDocXml[doc.id])
  const [raw, setRaw] = useState(false)
  const [query, setQuery] = useState('')
  useEffect(() => {
    if (xml === undefined) send({ type: 'getSpiDoc', id: doc.id })
  }, [doc.id, xml])
  const parsed = useMemo(() => (xml ? new DOMParser().parseFromString(xml, 'application/xml') : null), [xml])
  const root = parsed?.documentElement
  const error = parsed?.getElementsByTagName('parsererror')[0]
  return (
    <div className="spi-detail">
      <div className="spi-detail-head">
        <button className="icon-btn spi-back" onClick={onBack} aria-label="back">
          <ArrowLeft />
        </button>
        <div className="spi-title">
          <b>{doc.kind === 'epg' ? t('spi_pi') : doc.kind === 'serviceInformation' ? t('spi_si') : doc.kind}</b>
          <span className="hint">
            {doc.source} · {doc.scope} · {fmtSize(doc.size)} · {whenShort(doc.received)}
          </span>
          <span className="hint spi-url">{doc.url}</span>
        </div>
        <div className="seg small">
          <button className={!raw ? 'on' : ''} onClick={() => setRaw(false)}>
            {t('spi_decoded')}
          </button>
          <button className={raw ? 'on' : ''} onClick={() => setRaw(true)}>
            XML
          </button>
        </div>
      </div>
      {!raw && root && !error && (
        <div className="st-search spi-search">
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('search_station')} />
        </div>
      )}
      <div className="spi-body">
        {xml === undefined && <div className="hint">…</div>}
        {xml === null && <div className="hint">{t('spi_missing')}</div>}
        {error && <div className="notice error">{error.textContent}</div>}
        {raw && parsed && <pre className="spi-xml">{error ? xml : prettyXml(parsed)}</pre>}
        {!raw && root && !error && root.localName === 'serviceInformation' && <ServiceInfo root={root} query={query} />}
        {!raw && root && !error && root.localName === 'epg' && <Programmes root={root} query={query} />}
        {!raw && root && !error && !['serviceInformation', 'epg'].includes(root.localName) && <pre className="spi-xml">{prettyXml(parsed!)}</pre>}
      </div>
    </div>
  )
}

/**
 * The SPI diagnostics of the multiplex listened to: what in its carousel
 * goes against the standard, the worst first (what may hang receivers in red).
 */
function SpiHealth({ issues }: { issues: SpiIssue[] }) {
  const t = useT()
  const line = (i: SpiIssue) => {
    let s = t('spih_' + i.code)
      .replace('{subject}', i.subject)
      .replace('{a}', String(i.a ?? ''))
      .replace('{b}', String(i.b ?? ''))
      .replace('{text}', i.text ?? '')
    // (a size given in bits: an encoder that forgot to turn bits into bytes)
    if (i.code === 'bodySize' && i.a && i.b && i.a === i.b * 8) s += t('spih_bits')
    return s
  }
  return (
    <div className="spi-health">
      <div className="spi-group">
        {t('spih_title')}
        {!issues.length && <span className="spi-health-ok"> · {t('spih_ok')}</span>}
      </div>
      {issues.map((i, n) => (
        <div key={n} className={'spi-issue l' + i.level} title={i.level === 2 ? t('spih_level2') : undefined}>
          <span className="spi-issue-dot" />
          <span>{line(i)}</span>
        </div>
      ))}
    </div>
  )
}

export default function SpiView() {
  const t = useT()
  const docs = useStore((s) => s.spiDocs)
  const progress = useStore((s) => s.dataProgress)
  const [sel, setSel] = useState<string | null>(null)
  const [picture, setPicture] = useState<Picture | null>(null)
  useEffect(() => {
    send({ type: 'getSpiDocs' })
  }, [])
  const ueid = useStore((s) => s.ensemble.ueid ?? null)
  const health = useStore((s) => (ueid ? s.spiHealth[ueid] : undefined))
  const doc = docs.find((d) => d.id === sel)
  // the documents of the multiplex listened to first (its ECC and EId end
  // the scope of a broadcast one: "MUX A (E21234)"), then the others
  const air = docs.filter((d) => d.source === 'SPI')
  const here = (d: SpiDoc) => !!ueid && d.scope.endsWith(`(${ueid})`)
  const groups: [string, SpiDoc[]][] = [
    [t('spi_air_here'), air.filter(here)],
    [air.some(here) ? t('spi_air_other') : t('spi_air'), air.filter((d) => !here(d))],
    ['RadioDNS', docs.filter((d) => d.source !== 'SPI')],
  ]
  return (
    <OpenPicture.Provider value={setPicture}>
    {picture && (
      <div className="modal-back" onClick={() => setPicture(null)}>
        <div className="modal spi-picture" onClick={(e) => e.stopPropagation()}>
          <div className="modal-head">
            <b>{picture.size}</b>
            <span className="hint"> {picture.mime}</span>
            <span style={{ flex: 1 }} />
            <button className="icon-btn" onClick={() => setPicture(null)} aria-label="close">
              ✕
            </button>
          </div>
          <div className="logo-preview big">
            <img src={picture.src} alt="" />
          </div>
          <div className="hint spi-url">{picture.url}</div>
          <a className="btn" href={picture.src} download={picture.url.split('/').pop() || 'logo'} target="_blank" rel="noreferrer">
            {t('download')}
          </a>
        </div>
      </div>
    )}
    <div className={'spi-view' + (doc ? ' has-doc' : '')}>
      <div className="spi-list">
        {progress.length > 0 && (
          <div className="spi-progress">
            <div className="spi-group">{t('spi_receiving')}</div>
            {progress.map((p) => {
              // with a MOT directory the whole carousel, otherwise the object being received
              const pct = p.objects
                ? p.bytes ? Math.min(100, (100 * p.bytesDone) / p.bytes) : 0
                : p.segmentsTotal ? Math.min(100, (100 * (p.segments ?? 0)) / p.segmentsTotal) : 0
              return (
                <div key={p.sid + ':' + p.scid} className="spi-prog-item">
                  <div className="spi-prog-head">
                    <b>{p.label || p.sid}</b>
                    <span className="hint">
                      {p.objects
                        ? `${p.objectsDone}/${p.objects} · ${fmtSize(p.bytesDone)} / ${fmtSize(p.bytes)}`
                        : `${p.objectsDone} ${t('spi_objects')} · ${fmtSize(p.bytesDone)}`}
                    </span>
                  </div>
                  <div className="progress">
                    <div style={{ width: `${pct}%` }} />
                  </div>
                </div>
              )
            })}
          </div>
        )}
        {health && <SpiHealth issues={health} />}
        <div className="spi-list-head">
          <FileCode2 size={18} /> {t('spi_documents')}
        </div>
        {!docs.length && <div className="st-empty">{t('spi_none')}</div>}
        {groups.map(([title, list]) =>
          list.length ? (
            <div key={title}>
              <div className="spi-group">{title}</div>
              {list.map((d) => (
                <button key={d.id} className={'spi-item' + (d.id === sel ? ' on' : '')} onClick={() => setSel(d.id)}>
                  <span className={'spi-kind ' + d.kind}>{d.kind === 'epg' ? 'PI' : d.kind === 'serviceInformation' ? 'SI' : '?'}</span>
                  <span className="spi-item-txt">
                    <span className="spi-item-name">{d.scope}</span>
                    <span className="hint">
                      {d.items} {d.kind === 'epg' ? t('spi_programmes') : t('services').toLowerCase()} · {fmtSize(d.size)} · {whenShort(d.received)}
                    </span>
                  </span>
                  {d.source === 'SPI' ? <Radio size={15} className="hint" /> : null}
                </button>
              ))}
            </div>
          ) : null,
        )}
      </div>
      {doc ? (
        <DocDetail doc={doc} onBack={() => setSel(null)} />
      ) : (
        <div className="spi-detail spi-placeholder hint">{docs.length ? t('spi_pick') : ''}</div>
      )}
    </div>
    </OpenPicture.Provider>
  )
}
