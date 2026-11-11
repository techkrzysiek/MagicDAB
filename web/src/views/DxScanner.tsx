import { useEffect, useMemo, useRef, useState } from 'react'
import L from 'leaflet'
import { useStore, useChannelAllowed, fixedChannelOf } from '../store'
import { useT } from '../i18n'
import { send } from '../ws'
import { downloadText, kw, mhz } from '../util'
import { Flag, esc } from '../components/common'

function levelColor(level: number | null) {
  if (level === null) return '#9e9e9e'
  if (level >= -6) return '#22b3a6'
  if (level >= -15) return '#f5a524'
  return '#ef5d8a'
}

export default function DxScanner() {
  const t = useT()
  const hello = useStore((s) => s.hello)
  const results = useStore((s) => s.dxResults)
  const scan = useStore((s) => s.state?.scan)
  const fixed = useStore(fixedChannelOf)
  // (listeners: the channels they may tune to)
  const allowed = useChannelAllowed()
  const all = (hello?.channels ?? []).map((c) => c.name).filter((n) => !n.endsWith('N') && allowed(n))
  const [mode, setMode] = useState('normal')
  const [cycles, setCycles] = useState(1)
  const [channels, setChannels] = useState<string[]>(all)
  const [sel, setSel] = useState<number | null>(null)
  const mapDiv = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const layer = useRef<L.LayerGroup | null>(null)
  useEffect(() => {
    if (all.length && channels.length === 0) setChannels(all)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hello])

  const rows = useMemo(() => [...results].reverse(), [results])

  useEffect(() => {
    if (!mapDiv.current || map.current) return
    const loc = hello?.location
    const m = L.map(mapDiv.current).setView(loc ? [loc.lat, loc.lon] : [50, 19], loc ? 7 : 6)
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 18,
      attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
    }).addTo(m)
    layer.current = L.layerGroup().addTo(m)
    map.current = m
    setTimeout(() => m.invalidateSize(), 50)
    return () => {
      m.remove()
      map.current = null
    }
  }, [hello?.location])

  useEffect(() => {
    const lg = layer.current
    if (!lg) return
    lg.clearLayers()
    const loc = hello?.location
    if (loc) L.circleMarker([loc.lat, loc.lon], { radius: 7, color: '#fff', fillColor: '#6d3fd8', fillOpacity: 1, weight: 3 }).addTo(lg)
    const seen = new Set<string>()
    results.forEach((r, i) => {
      if (r.lat === null || r.lon === null) return
      const key = `${r.ueid}-${r.main}-${r.sub}`
      if (seen.has(key)) return
      seen.add(key)
      L.circleMarker([r.lat, r.lon], { radius: sel === i ? 9 : 6, color: '#fff', weight: 1.5, fillColor: levelColor(r.level), fillOpacity: 0.9 })
        .bindTooltip(esc(`${r.channel} ${r.label} ${r.main}-${r.sub} ${r.location ?? ''}`))
        .on('click', () => setSel(i))
        .addTo(lg)
    })
  }, [results, sel, hello?.location])

  const csv = () =>
    [
      'Time;Channel;Frequency;UEID;Label;Services;SNR;Main;Sub;Level;Location;Power;Distance;Azimuth',
      ...results.map((r) =>
        [new Date(r.time * 1000).toISOString(), r.channel, r.frequency, r.ueid, r.label, r.services, r.snr, r.main ?? '', r.sub ?? '', r.level ?? '', r.location ?? '', r.power ?? '', r.distance ?? '', r.azimuth ?? ''].join(';'),
      ),
    ].join('\n')

  const running = scan?.dx
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10, minHeight: 0 }}>
      <p className="hint" style={{ margin: 0 }}>{t('dx_info')}</p>
      <div className="form" style={{ maxWidth: 'none' }}>
        <label>{t('mode')}</label>
        <div className="row">
          <select value={mode} onChange={(e) => setMode(e.target.value)} disabled={!!running}>
            <option value="fast">{t('mode_fast')}</option>
            <option value="normal">{t('mode_normal')}</option>
            <option value="precise">{t('mode_precise')}</option>
          </select>
          <label style={{ marginLeft: 12 }}>{t('cycles')}</label>
          <input type="number" min={0} value={cycles} style={{ width: 70 }} onChange={(e) => setCycles(parseInt(e.target.value) || 0)} disabled={!!running} />
          {running ? (
            <button className="btn danger" onClick={() => send({ type: 'dxScan', action: 'stop' })}>
              {t('stop')}
            </button>
          ) : (
            <button className="btn primary" disabled={!channels.length || !!fixed} title={fixed ? t('channel_fixed') : undefined} onClick={() => send({ type: 'dxScan', action: 'start', mode, cycles, channels })}>
              {t('start')}
            </button>
          )}
          {running && (
            <span>
              {t('cycle')} {running.cycle}
              {running.cycles ? `/${running.cycles}` : ''} · {scan?.channel} ({(scan?.index ?? 0) + 1}/{scan?.total})
            </span>
          )}
        </div>
        <label>{t('select_channels')}</label>
        <div className="row" style={{ gap: 4 }}>
          <button className="btn" onClick={() => setChannels(all)}>
            {t('all')}
          </button>
          <button className="btn" onClick={() => setChannels([])}>
            {t('none')}
          </button>
          {all.map((c) => (
            <button
              key={c}
              className={'chan-chip' + (channels.includes(c) ? ' on' : '')}
              onClick={() => setChannels(channels.includes(c) ? channels.filter((x) => x !== c) : [...channels, c])}
            >
              {c}
            </button>
          ))}
        </div>
      </div>
      <div className="dx-map" ref={mapDiv} />
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <b>
          {t('results')} ({results.length})
        </b>
        <span style={{ flex: 1 }} />
        <button className="btn" disabled={!results.length} onClick={() => downloadText('magicdab-dx.csv', csv())}>
          {t('export_csv')}
        </button>
        <button className="btn" disabled={!results.length} onClick={() => send({ type: 'dxScan', action: 'clear' })}>
          {t('clear')}
        </button>
      </div>
      <div style={{ overflow: 'auto', maxHeight: 420 }}>
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th>{t('time')}</th>
                <th>{t('channel')}</th>
                <th>{t('frequency')}</th>
                <th>UEID</th>
                <th>{t('ensemble')}</th>
                <th>{t('services')}</th>
                <th>SNR</th>
                <th>TII</th>
                <th>{t('level')}</th>
                <th>{t('location')}</th>
                <th>{t('power')}</th>
                <th>{t('distance')}</th>
                <th>{t('azimuth')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, j) => {
                const i = results.length - 1 - j
                return (
                  <tr key={i} className={sel === i ? 'sel' : ''} onClick={() => setSel(i)}>
                    <td>{new Date(r.time * 1000).toLocaleTimeString()}</td>
                    <td>{r.channel}</td>
                    <td>{mhz(r.frequency)}</td>
                    <td>
                      <Flag code={r.country} size={12} /> {r.ueid}
                    </td>
                    <td>{r.label}</td>
                    <td>{r.services}</td>
                    <td>{r.snr.toFixed(1)} dB</td>
                    <td>{r.main !== null ? `${r.main}-${r.sub}` : ''}</td>
                    <td>{r.level !== null ? `${r.level.toFixed(1)} dB` : ''}</td>
                    <td>{r.location ?? ''}</td>
                    <td>{r.power !== null ? `${kw(r.power)} kW` : ''}</td>
                    <td>{r.distance !== null ? `${r.distance.toFixed(1)} km` : ''}</td>
                    <td>{r.azimuth !== null ? `${r.azimuth.toFixed(0)}°` : ''}</td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
