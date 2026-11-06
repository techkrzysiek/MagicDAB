import { useEffect, useMemo, useRef, useState } from 'react'
import L from 'leaflet'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader, esc } from '../components/common'
import { mhz } from '../util'
import type { TiiCode } from '../types'

// A transmitter's own signal: the SNR of the multiplex plus its TII level
// (against the strongest, 0 dB), coloured with the SNR thresholds of the
// signal bars (below 7 dB weak, below 10 dB fair, from 10 dB good); white
// with a grey rim: known but not heard now (a layer of its own, to hide).
function cssColor(name: string, fallback: string) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
  return v || fallback
}
function txSnr(snr: number | null, level: number | null) {
  return snr === null || level === null ? null : snr + level
}
function levelColor(est: number | null) {
  if (est === null) return '#9e9e9e'
  if (est < 7) return cssColor('--sig-low', '#ef4f5f')
  if (est < 10) return cssColor('--sig-mid', '#f5a524')
  return cssColor('--sig-good', '#22b37a')
}

interface Row {
  code: string
  level: number | null
  distance: number | null
  azimuth: number | null
  location: string
  lat: number | null
  lon: number | null
  power: number | null
  altitude: number | null
  height: number | null
  detected: boolean
}

function TiiPlot({ pairs }: { pairs: number[] }) {
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const c = ref.current
    if (!c) return
    const w = (c.width = c.clientWidth * devicePixelRatio)
    const h = (c.height = c.clientHeight * devicePixelRatio)
    const g = c.getContext('2d')!
    g.clearRect(0, 0, w, h)
    if (!pairs.length) return
    const max = Math.max(...pairs, 1e-9)
    const bw = w / pairs.length
    for (let i = 0; i < pairs.length; i++) {
      const v = pairs[i] / max
      const comb = i % 24
      g.fillStyle = `hsl(${(comb * 15) % 360} 70% 60%)`
      g.fillRect(i * bw, h - v * (h - 12), Math.max(1, bw - 0.5), v * (h - 12))
    }
    g.fillStyle = '#c9ccd1'
    g.font = `${10 * devicePixelRatio}px sans-serif`
    g.fillText('TII', 4 * devicePixelRatio, 12 * devicePixelRatio)
  }, [pairs])
  return <canvas ref={ref} style={{ width: '100%', height: '100%' }} />
}

export default function TiiView() {
  const t = useT()
  const tii = useStore((s) => s.tii)
  // (the multiplex' SNR: each transmitter's signal is it plus its TII level)
  // (to half a dB: the map is not redrawn at every small change)
  const snr = useStore((s) => (s.signal && s.signal.synced ? Math.round(s.signal.snr * 2) / 2 : null))
  const pairs = useStore((s) => s.tiiPairs)
  const ens = useStore((s) => s.ensemble)
  const hello = useStore((s) => s.hello)
  const mapDiv = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const layer = useRef<L.LayerGroup | null>(null)
  const unheard = useRef<L.LayerGroup | null>(null)
  const fitted = useRef<string>('')
  const [selected, setSelected] = useState<string | null>(null)
  const [seen, setSeen] = useState<Record<string, TiiCode>>({})
  const [sortKey, setSortKey] = useState<'level' | 'code' | 'distance'>('level')

  // remember codes detected since the view was opened (per ensemble)
  useEffect(() => {
    setSeen({})
    setSelected(null)
  }, [tii?.eid, tii?.frequency])
  useEffect(() => {
    if (!tii) return
    setSeen((prev) => {
      const n = { ...prev }
      for (const c of tii.codes) n[c.code] = c
      return n
    })
  }, [tii])

  const rows: Row[] = useMemo(() => {
    const detected = new Set((tii?.codes ?? []).map((c) => c.code))
    const out: Row[] = []
    for (const c of Object.values(seen)) {
      const now = tii?.codes.find((x) => x.code === c.code)
      out.push({
        code: c.code,
        level: now ? now.level : null,
        distance: c.distance,
        azimuth: c.azimuth,
        location: c.tx?.location ?? '',
        lat: c.tx?.lat ?? null,
        lon: c.tx?.lon ?? null,
        power: c.tx?.powerKw ?? null,
        altitude: c.tx?.altitude ?? null,
        height: c.tx?.height ?? null,
        detected: detected.has(c.code),
      })
    }
    const sorted = out.sort((a, b) => {
      if (sortKey === 'code') return a.code.localeCompare(b.code, undefined, { numeric: true })
      if (sortKey === 'distance') return (a.distance ?? 1e9) - (b.distance ?? 1e9)
      if (a.detected !== b.detected) return a.detected ? -1 : 1
      return (b.level ?? -999) - (a.level ?? -999)
    })
    return sorted
  }, [seen, tii, sortKey])

  // map setup
  useEffect(() => {
    if (!mapDiv.current || map.current) return
    const loc = hello?.location
    const m = L.map(mapDiv.current, { zoomControl: true, attributionControl: true }).setView(loc ? [loc.lat, loc.lon] : [50.0, 19.0], loc ? 8 : 6)
    // topographic by default (hills and valleys explain a path), the plain map to choose
    const osm = L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 18,
      attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
    })
    const topo = L.tileLayer('https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png', {
      maxZoom: 17,
      attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors, SRTM | © <a href="https://opentopomap.org">OpenTopoMap</a> (CC-BY-SA)',
    })
    const base: Record<string, L.TileLayer> = { [t('tii_map_topo')]: topo, [t('tii_map_plain')]: osm }
    let chosen = 'topo'
    try {
      chosen = localStorage.getItem('magicdab-tii-map') || 'topo'
    } catch {
      // (no stored choice)
    }
    ;(chosen === 'plain' ? osm : topo).addTo(m)
    // the transmitters not heard now, shown unless hidden
    const off = L.layerGroup()
    let hide = false
    try {
      hide = localStorage.getItem('magicdab-tii-unheard') === 'hide'
    } catch {
      // (no stored choice)
    }
    if (!hide) off.addTo(m)
    // (top left under the zoom: the right side holds the transmitters' panel)
    L.control.layers(base, { [t('tii_unheard')]: off }, { position: 'topleft' }).addTo(m)
    m.on('baselayerchange', (e: L.LayersControlEvent) => {
      try {
        localStorage.setItem('magicdab-tii-map', e.layer === osm ? 'plain' : 'topo')
      } catch {
        // (not kept)
      }
    })
    const keep = (v: string) => () => {
      try {
        localStorage.setItem('magicdab-tii-unheard', v)
      } catch {
        // (not kept)
      }
    }
    m.on('overlayadd', keep('show'))
    m.on('overlayremove', keep('hide'))
    layer.current = L.layerGroup().addTo(m)
    unheard.current = off
    map.current = m
    setTimeout(() => m.invalidateSize(), 50)
    return () => {
      m.remove()
      map.current = null
    }
  }, [hello?.location])

  // markers
  useEffect(() => {
    const m = map.current
    const lg = layer.current
    const off = unheard.current
    if (!m || !lg || !off) return
    lg.clearLayers()
    off.clearLayers()
    const pts: L.LatLngExpression[] = []
    const loc = hello?.location
    if (loc) {
      L.circleMarker([loc.lat, loc.lon], { radius: 7, color: '#ffffff', fillColor: '#6d3fd8', fillOpacity: 1, weight: 3 }).addTo(lg)
      pts.push([loc.lat, loc.lon])
    }
    // known but not detected transmitters of this ensemble: white dots, a dark rim
    for (const k of tii?.known ?? []) {
      if (rows.some((r) => r.code === k.code)) continue
      L.circleMarker([k.lat, k.lon], { radius: 5, color: '#4a4f57', weight: 2, fillColor: '#ffffff', fillOpacity: 0.95 })
        .bindTooltip(esc(`${k.code} ${k.location}`))
        .addTo(off)
    }
    // lines from the receiver to the transmitters heard, in the colour of their level
    if (loc) {
      for (const r of rows) {
        if (r.lat === null || r.lon === null || !r.detected) continue
        L.polyline(
          [
            [loc.lat, loc.lon],
            [r.lat, r.lon],
          ],
          { color: levelColor(txSnr(snr, r.level)), weight: selected === r.code ? 4 : 2, opacity: selected === r.code ? 0.95 : 0.7, interactive: false },
        ).addTo(lg)
      }
    }
    for (const r of rows) {
      if (r.lat === null || r.lon === null) continue
      // (its signal under the code: the multiplex' SNR plus its TII level)
      const est = r.detected ? txSnr(snr, r.level) : null
      const db = est !== null ? `<span class="db">${est.toFixed(1)} dB</span>` : r.detected && r.level !== null ? `<span class="db">${r.level.toFixed(1)} dB</span>` : ''
      const icon = L.divIcon({
        className: '',
        html: r.detected
          ? `<div class="tii-marker${selected === r.code ? ' sel' : ''}" style="background:${levelColor(est)}"><b>${esc(r.code)}</b>${db}</div>`
          : `<div class="tii-marker off${selected === r.code ? ' sel' : ''}"><b>${esc(r.code)}</b></div>`,
        iconSize: [0, 0],
      })
      L.marker([r.lat, r.lon], { icon })
        .on('click', () => setSelected(r.code))
        .addTo(r.detected ? lg : off)
      pts.push([r.lat, r.lon])
    }
    const key = (tii?.eid ?? '') + rows.filter((r) => r.lat !== null).length
    if (pts.length > 1 && fitted.current !== key) {
      fitted.current = key
      m.fitBounds(L.latLngBounds(pts), { padding: [60, 60], maxZoom: 10 })
    }
  }, [rows, selected, hello?.location, tii?.known, tii?.eid, snr])

  const sel = rows.find((r) => r.code === selected)
  const noDb = (hello?.txdb.count ?? 0) === 0

  return (
    <>
      <ViewHeader />
      <div className="tii-view">
        <div className="tii-map" ref={mapDiv} />
        {(noDb || !hello?.location) && <div className="tii-msg">{noDb ? t('tii_no_db') : t('tii_no_location')}</div>}
        <div className="tii-table">
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th onClick={() => setSortKey('code')}>{t('code')}</th>
                  <th onClick={() => setSortKey('level')} title={t('tii_level_hint')}>{t('level')} ▾</th>
                  <th title={t('tii_snr_hint')}>SNR ≈</th>
                  <th onClick={() => setSortKey('distance')}>{t('distance')}</th>
                  <th>{t('azimuth')}</th>
                  <th>{t('location')}</th>
                </tr>
              </thead>
              <tbody>
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={5} className="hint">
                      {t('tii_none')}
                    </td>
                  </tr>
                )}
                {rows.map((r) => (
                  <tr key={r.code} className={(selected === r.code ? 'sel ' : '') + (r.detected ? '' : 'dim')} onClick={() => setSelected(r.code)}>
                    <td>{r.code}</td>
                    <td>{r.level !== null ? `${r.level.toFixed(1)} dB` : ''}</td>
                    <td>
                      {txSnr(snr, r.level) !== null && (
                        <span className="tii-snr" style={{ color: levelColor(txSnr(snr, r.level)) }}>
                          {txSnr(snr, r.level)!.toFixed(1)} dB
                        </span>
                      )}
                    </td>
                    <td>{r.distance !== null ? `${r.distance.toFixed(1)} km` : ''}</td>
                    <td>{r.azimuth !== null ? `${r.azimuth.toFixed(0)}°` : ''}</td>
                    <td>{r.location}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
        <div className="tii-details">
          <div>
            {t('ensemble')}: <b>{ens.label ?? '-'}</b>
          </div>
          <div>
            {t('channel')}: <b>{ens.channel ?? '-'}</b> ({ens.frequency ? `${ens.frequency} kHz` : '-'})
          </div>
          <div>
            ECC: <b>{ens.eccHex ?? '-'}</b> | EID: <b>{ens.eidHex ?? '-'}</b> | TX: <b>{tii?.codes.length ?? 0}</b>
          </div>
          {sel && (
            <div style={{ borderTop: '1px solid var(--divider-soft)', marginTop: 6, paddingTop: 6 }}>
              <div>
                <b>{sel.location || sel.code}</b>
              </div>
              {sel.power !== null && (
                <div>
                  ERP: <b>{sel.power} kW</b>
                </div>
              )}
              {sel.altitude !== null && (
                <div>
                  Altitude: <b>{sel.altitude} m + {sel.height} m</b>
                </div>
              )}
              {sel.lat !== null && (
                <div>
                  GPS: <b>{sel.lat.toFixed(5)}° N, {sel.lon?.toFixed(5)}° E</b>
                </div>
              )}
              {sel.distance !== null && (
                <div>
                  {t('distance')}: <b>{sel.distance.toFixed(1)} km</b>, {t('azimuth')}: <b>{sel.azimuth?.toFixed(1)}°</b>
                </div>
              )}
            </div>
          )}
          {ens.frequency ? <div className="hint">{mhz(ens.frequency)} MHz</div> : null}
        </div>
        <div className="tii-plot">
          <TiiPlot pairs={pairs} />
        </div>
      </div>
    </>
  )
}
