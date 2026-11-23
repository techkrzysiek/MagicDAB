import { useEffect, useMemo, useRef, useState } from 'react'
import L from 'leaflet'
import { TriangleAlert } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { setSubscriptions } from '../ws'
import { esc } from '../components/common'
import type { TrafficEvent } from '../types'

/** Colour of a message on the map and in the list: closed roads red, accidents
 *  and dangers orange, roadworks amber, the rest blue. */
function tone(e: TrafficEvent) {
  const causes = e.causes.map((c) => c.name.toLowerCase()).join(' ')
  if (e.effect?.code === 7) return '#e5484d'
  if (/accident|danger|object|ghost|wrong/.test(causes)) return '#f0883e'
  if (/roadwork|construction|maintenance/.test(causes)) return '#f5a524'
  return '#4a9df8'
}

/** Its line (or point) on the map, [lat, lon]. */
function geometry(e: TrafficEvent): [number, number][] {
  const l = e.location
  if (!l) return []
  if (l.shape && l.shape.length > 1) return l.shape
  if (l.points.length > 1) return l.points
  if (l.point) return [l.point]
  return l.points
}

function title(e: TrafficEvent) {
  const c = e.causes[0]
  return (c?.subName ?? c?.name ?? e.effect?.name ?? e.application) || e.key
}

const time = (s?: number | null) => (s ? new Date(s * 1000).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' }) : '')

const km = (m: number) => (m >= 1000 ? `${(m / 1000).toFixed(m >= 10000 ? 0 : 1)} km` : `${m} m`)
const kmh = (ms: number) => Math.round(ms * 3.6)
const coords = (p: [number, number]) => `${p[0].toFixed(5)}, ${p[1].toFixed(5)}`

/** Everything a message says, under it when it is selected. */
function Details({ e }: { e: TrafficEvent }) {
  const t = useT()
  const l = e.location
  const rows: [string, React.ReactNode][] = [
    [
      t('tr_causes'),
      e.causes.length ? (
        <span className="tr-lines">
          {e.causes.map((c, i) => (
            <span key={i}>
              {c.name}
              {c.subName ? `: ${c.subName}` : ''}
              {c.warning ? `, ${c.warning.name}` : ''}
              {c.lengthAffected ? `, ${km(c.lengthAffected)}` : ''}
              {c.laneRestriction ? `, ${c.laneRestriction.name}${c.lanes ? ` (${t('tr_lanes').replace('{n}', String(c.lanes))})` : ''}` : ''}
              {c.unverified ? `, ${t('tr_unverified')}` : ''}
              {c.text?.length ? `: ${c.text.join(' ')}` : ''}
            </span>
          ))}
        </span>
      ) : null,
    ],
    [t('tr_effect'), e.effect?.name],
    [t('tr_warning'), e.warning?.name],
    [t('tr_tendency'), e.tendency?.name],
    [t('tr_delay'), e.delay ? `${e.delay} min` : null],
    [t('tr_length'), e.lengthAffected ? km(e.lengthAffected) : null],
    [
      t('tr_speed'),
      [e.averageSpeed ? t('tr_avg').replace('{v}', String(kmh(e.averageSpeed))) : '', e.expectedSpeed ? t('tr_expected').replace('{v}', String(kmh(e.expectedSpeed))) : '']
        .filter(Boolean)
        .join(', ') || null,
    ],
    [
      t('tr_limits'),
      e.speedLimits?.length
        ? e.speedLimits
            .map((s) => `${s.value} ${s.mph ? 'mph' : 'km/h'}${s.wet ? ` (${t('tr_wet').replace('{v}', String(s.wet))})` : ''}${s.length ? `, ${km(s.length)}` : ''}`)
            .join('; ')
        : null,
    ],
    [
      t('tr_advice'),
      e.advice?.length ? (
        <span className="tr-lines">
          {e.advice.map((a, i) => (
            <span key={i}>
              {[a.name, a.subName].filter(Boolean).join(': ')}
              {a.text?.length ? ` ${a.text.join(' ')}` : ''}
            </span>
          ))}
        </span>
      ) : null,
    ],
    [
      t('tr_vehicles'),
      e.restrictions?.length
        ? e.restrictions
            .map((r) => [r.vehicle?.name, ...r.conditions.map((c) => `${c.name}${c.value != null ? ` ${c.value}` : ''}`)].filter(Boolean).join(', '))
            .join('; ')
        : null,
    ],
    [t('tr_diversions'), e.diversionRoutes ? t('tr_routes').replace('{n}', String(e.diversionRoutes)) : null],
    [t('tr_time'), e.start || e.stop ? `${time(e.start)} – ${time(e.stop)}` : null],
    [t('tr_valid'), e.expires ? time(e.expires) : null],
    [t('tr_sent'), e.generated ? time(e.generated) : null],
    [
      t('tr_place'),
      l ? (
        <span className="tr-lines">
          <span>
            {[l.method, l.kind].filter(Boolean).join(', ')}
            {l.tmc ? `, TMC ${l.tmc.table}:${l.tmc.location}${l.tmc.bothDirections ? `, ${t('tr_both')}` : ''}` : ''}
          </span>
          {geometry(e).length ? (
            <span className="mono">
              {coords(geometry(e)[0])}
              {geometry(e).length > 1 ? ` → ${coords(geometry(e)[geometry(e).length - 1])}` : ''}
            </span>
          ) : null}
          {l.text?.length ? <span>{l.text.join(' ')}</span> : null}
        </span>
      ) : null,
    ],
  ]
  return (
    <span className="tr-details">
      {rows
        .filter(([, v]) => v !== null && v !== undefined && v !== '')
        .map(([k, v]) => (
          <span key={k} className="tr-row">
            <span className="k">{k}</span>
            <span>{v}</span>
          </span>
        ))}
    </span>
  )
}

/** TPEG traffic messages of the multiplex (a data service, UA 0x004): a list and a map. */
export default function TrafficView() {
  const t = useT()
  const events = useStore((s) => s.tpeg)
  const [q, setQ] = useState('')
  const [sel, setSel] = useState<string | null>(null)
  const mapDiv = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const layer = useRef<L.LayerGroup | null>(null)
  const fitted = useRef(false)

  const list = useMemo(() => {
    const needle = q.trim().toLowerCase()
    return Object.values(events)
      .filter((e) => {
        if (!needle) return true
        const hay = [title(e), e.effect?.name, ...e.causes.map((c) => c.name), e.location?.tmc ? `${e.location.tmc.table}:${e.location.tmc.location}` : '']
        return hay.some((h) => h?.toLowerCase().includes(needle))
      })
      .sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0) || title(a).localeCompare(title(b)))
  }, [events, q])

  // (the server sends the messages to the pages that show them)
  useEffect(() => {
    setSubscriptions({ traffic: true })
    return () => setSubscriptions({ traffic: false })
  }, [])

  // the map: once
  useEffect(() => {
    if (!mapDiv.current || map.current) return
    const m = L.map(mapDiv.current, { zoomControl: true, attributionControl: true }).setView([50, 10], 6)
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 18,
      attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
    }).addTo(m)
    layer.current = L.layerGroup().addTo(m)
    // (a click beside the messages: none selected any more)
    m.on('click', () => setSel(null))
    map.current = m
    return () => {
      m.remove()
      map.current = null
    }
  }, [])

  // the messages on it
  useEffect(() => {
    const g = layer.current
    if (!g || !map.current) return
    g.clearLayers()
    const all: L.LatLngExpression[] = []
    for (const e of list) {
      const pts = geometry(e)
      if (!pts.length) continue
      const color = tone(e)
      const weight = e.key === sel ? 9 : 5
      const shape = pts.length > 1 ? L.polyline(pts, { color, weight, opacity: 0.85 }) : L.circleMarker(pts[0], { color, radius: e.key === sel ? 9 : 6, fillOpacity: 0.85 })
      // (a tooltip is HTML: the text escaped)
      shape.bindTooltip(esc(`${title(e)}${e.effect ? `, ${e.effect.name}` : ''}`))
      shape.on('click', (ev) => {
        L.DomEvent.stopPropagation(ev)
        setSel((cur) => (cur === e.key ? null : e.key))
      })
      shape.addTo(g)
      all.push(...pts)
    }
    if (!fitted.current && all.length) {
      map.current.fitBounds(L.latLngBounds(all).pad(0.05))
      fitted.current = true
    }
  }, [list, sel])

  // a tap selects a message and zooms to it; another tap on it (or Esc) lets
  // it go and shows them all again
  const showAll = () => {
    const all = list.flatMap(geometry)
    if (all.length && map.current) map.current.fitBounds(L.latLngBounds(all).pad(0.05))
  }
  const pick = (e: TrafficEvent) => {
    if (sel === e.key) {
      setSel(null)
      showAll()
      return
    }
    setSel(e.key)
    const pts = geometry(e)
    if (pts.length && map.current) map.current.fitBounds(L.latLngBounds(pts).pad(0.6), { maxZoom: 14 })
  }
  useEffect(() => {
    if (!sel) return
    const key = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') setSel(null)
    }
    document.addEventListener('keydown', key)
    return () => document.removeEventListener('keydown', key)
  }, [sel])

  return (
    <div className="traffic-body">
        <div className="traffic-list">
          <div className="traffic-head">
            <b>{t('traffic_n').replace('{n}', String(Object.keys(events).length))}</b>
            <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('traffic_search')} />
          </div>
          {!list.length && <div className="hint">{t('traffic_none')}</div>}
          {list.map((e) => (
            <button key={e.key} className={'traffic-item' + (e.key === sel ? ' selected' : '')} onClick={() => pick(e)}>
              <span className="traffic-dot" style={{ background: tone(e) }} />
              <span className="traffic-txt">
                <b>{title(e)}</b>
                <span>
                  {[e.effect?.name, e.warning?.name, e.delay ? `+${e.delay} min` : '', e.location?.tmc ? `TMC ${e.location.tmc.table}:${e.location.tmc.location}` : '']
                    .filter(Boolean)
                    .join(', ')}
                </span>
                {e.stop && e.key !== sel ? <span className="hint">{t('traffic_until').replace('{t}', time(e.stop))}</span> : null}
                {e.key === sel && <Details e={e} />}
              </span>
              {e.warning && e.warning.code >= 2 && <TriangleAlert size={16} className="traffic-warn" />}
            </button>
          ))}
        </div>
      <div className="traffic-map" ref={mapDiv} />
    </div>
  )
}
