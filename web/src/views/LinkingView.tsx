import { useEffect, useRef, useState } from 'react'
import { RadioReceiver, RefreshCw, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { linkingCommand } from '../actions'
import FmView from './FmView'

const mhz = (khz?: number) => (khz ? (khz / 1000).toFixed(1) + ' MHz' : '—')

/** Small line plot on a canvas; `mark` draws a vertical line at that index. */
function Plot({ data, mark, height = 90, points }: { data: number[]; mark?: number; height?: number; points?: { x: number; y: number; ok: boolean }[] }) {
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const c = ref.current
    if (!c) return
    const dpr = window.devicePixelRatio || 1
    const w = (c.width = c.clientWidth * dpr)
    const h = (c.height = height * dpr)
    const g = c.getContext('2d')!
    const css = getComputedStyle(c)
    g.clearRect(0, 0, w, h)
    g.strokeStyle = css.getPropertyValue('--plot-grid') || '#444'
    g.beginPath()
    g.moveTo(0, h - 0.5)
    g.lineTo(w, h - 0.5)
    g.stroke()
    const accent = css.getPropertyValue('--accent') || '#a68bff'
    if (points) {
      if (!points.length) return
      const ys = points.map((p) => p.y)
      const lo = Math.min(...ys), hi = Math.max(...ys)
      const span = Math.max(hi - lo, 1)
      const x0 = points[0].x, x1 = points[points.length - 1].x
      for (const p of points) {
        const x = ((p.x - x0) / Math.max(x1 - x0, 1)) * (w - 8 * dpr) + 4 * dpr
        const y = h - 6 * dpr - ((p.y - lo) / span) * (h - 12 * dpr)
        g.fillStyle = p.ok ? accent : css.getPropertyValue('--text-disabled') || '#777'
        g.fillRect(x - 2 * dpr, y - 2 * dpr, 4 * dpr, 4 * dpr)
      }
      g.fillStyle = css.getPropertyValue('--text-secondary') || '#aaa'
      g.font = `${11 * dpr}px sans-serif`
      g.fillText(`${hi.toFixed(0)} ms`, 4 * dpr, 12 * dpr)
      g.fillText(`${lo.toFixed(0)} ms`, 4 * dpr, h - 4 * dpr)
      return
    }
    if (!data.length) return
    const lo = Math.min(...data), hi = Math.max(...data)
    const span = Math.max(hi - lo, 1e-9)
    g.strokeStyle = accent
    g.lineWidth = 1.5 * dpr
    g.beginPath()
    data.forEach((v, i) => {
      const x = (i / Math.max(data.length - 1, 1)) * w
      const y = h - 4 * dpr - ((v - lo) / span) * (h - 8 * dpr)
      if (i) g.lineTo(x, y)
      else g.moveTo(x, y)
    })
    g.stroke()
    if (mark != null) {
      const x = (mark / Math.max(data.length - 1, 1)) * w
      g.strokeStyle = css.getPropertyValue('--text-secondary') || '#aaa'
      g.setLineDash([3 * dpr, 3 * dpr])
      g.beginPath()
      g.moveTo(x, 0)
      g.lineTo(x, h)
      g.stroke()
    }
  }, [data, mark, height, points])
  return <canvas ref={ref} className="link-plot" style={{ height }} />
}

/** What the FM channels look like against DAB: mid (L+R) and side (L-R) polarity. */
function fmMismatch(polarity: number, side: number, short = false): string {
  const k = polarity < 0 && side < 0 ? 'swapped_inverted' : side < 0 ? 'swapped' : polarity < 0 ? 'inverted' : 'normal'
  return (short ? 'link_polarity_' : 'link_fm_') + k
}

export default function LinkingView() {
  const t = useT()
  const s = useStore((x) => x.linking)
  // the FM tuner only helps the linking: it opens over this view
  const [fmOpen, setFmOpen] = useState(false)
  if (!s) return <div className="view link-view"><div className="fm-empty">…</div></div>
  const l = s.linker
  const m = s.measurement
  const phaseName = t(('link_phase_' + s.phase.name) as never) || s.phase.name
  const piOk = s.fm.pi != null && s.targets.some((x) => x.pi === s.fm.pi)
  const time = (ms: number) => new Date(ms).toLocaleTimeString()
  return (
    <div className="view link-view">
      <div className="link-head">
        <div className="title">{t('link_title')}</div>
        <div className="seg">
          {(['auto', 'dab', 'fm'] as const).map((mode) => (
            <button key={mode} className={s.mode === mode ? 'on' : ''} onClick={() => linkingCommand({ mode })}>
              {mode.toUpperCase()}
            </button>
          ))}
        </div>
        <label className="check">
          <input type="checkbox" checked={!!l?.ab} onChange={(e) => linkingCommand({ ab: e.target.checked })} /> {t('link_ab')}
          {l?.ab && <span className="hint"> · {t(l.abFm ? 'link_ab_fm' : 'link_ab_dab')}</span>}
        </label>
        <label className="check">
          <input type="checkbox" checked={s.enabled} onChange={(e) => linkingCommand({ enabled: e.target.checked })} /> {t('link_enabled')}
        </label>
        <button className="btn" onClick={() => linkingCommand({ search: true })}>
          <RefreshCw size={15} /> {t('link_search')}
        </button>
        <button className="btn" onClick={() => setFmOpen(true)}>
          <RadioReceiver size={15} /> {t('fm_tuner')}
        </button>
      </div>
      {s.reason && <div className="hint">{s.reason}</div>}
      {l?.fmOnly && <div className="banner">{t('link_fm_only')}</div>}
      {l && (l.polarity < 0 || (l.sidePolarity ?? 1) < 0) && (
        // the receiving side does not match DAB (the tuner, its firmware or the
        // FM chain): linking corrects it, the admin should know
        <div className="banner">{t(fmMismatch(l.polarity, l.sidePolarity ?? 1))}</div>
      )}
      {l?.fmAheadMs != null && l.fmAheadMs < 0 && (
        <div className="banner">
          {t('link_fm_late')
            .replace('{late}', (-l.fmAheadMs / 1000).toFixed(1))
            .replace('{wait}', ((l.dabExtraMs ?? 0) / 1000).toFixed(1))}
        </div>
      )}
      <div className="link-cards">
        <div className="link-card">
          <div className="k">DAB</div>
          <div className="v">{s.service ? `${s.service.label} (${s.service.sid})` : '—'}</div>
          <div className="row2">
            <span className={'dot ' + (s.dab.good ? 'good' : s.dab.bad ? 'bad' : 'mid')} />
            {s.dab.synced ? `SNR ${s.dab.snr.toFixed(1)} dB` : 'no sync'} · {t('link_au_errors')} {(s.dab.auErrorRate * 100).toFixed(1)}%
            {s.service?.ecc && ` · ECC ${s.service.ecc}`}
          </div>
        </div>
        <div className="link-card">
          <div className="k">FM</div>
          <div className="v">
            {mhz(s.fm.khz)} {s.fm.ps}
          </div>
          <div className="row2">
            <span className={'dot ' + (piOk ? 'good' : 'bad')} />
            PI {s.fm.pi ?? '—'}
            {'?'.repeat(s.fm.piErrors ?? 0)} · {s.fm.level.toFixed(1)} dBf
            {s.fm.antennas > 1 ? ` · ${t('fm_antenna')} ${s.fm.antenna + 1}` : ''} ·{' '}
            {s.phase.name === 'probing' ? (
              // reading the PI of every station of the band scan, one by one
              t('link_probing')
                .replace('{pi}', s.targets.map((x) => x.pi).join(', ') || '—')
                .replace('{f}', s.phase.khz ? mhz(s.phase.khz) : '…')
                .replace('{n}', String(s.phase.left ?? 0))
            ) : (
              <>
                {phaseName}
                {s.phase.khz ? ` ${mhz(s.phase.khz)}` : ''}
                {s.phase.antenna != null && s.fm.antennas > 1 ? ` / ${s.phase.antenna + 1}` : ''}
                {s.phase.total ? ` (${s.phase.done}/${s.phase.total})` : ''}
                {s.phase.left != null ? ` (${s.phase.left})` : ''}
              </>
            )}
          </div>
        </div>
        <div className="link-card">
          <div className="k">{t('link_output')}</div>
          <div className="v">{l?.source ? l.source.toUpperCase() : '—'} {l && !l.aligned ? `(${t('link_not_aligned')})` : l ? `(${t('link_aligned')})` : ''}</div>
          <div className="fade">
            <span>DAB</span>
            <div className="fm-bar">
              <div style={{ width: `${(l?.fade ?? 0) * 100}%` }} />
            </div>
            <span>FM</span>
          </div>
        </div>
      </div>
      {l && (
        <div className="link-stats">
          <div><span>{t('link_delay')}</span><b>{l?.fmAheadMs != null ? `${l.fmAheadMs.toFixed(0)} ms` : m ? `${m.delayMs.toFixed(0)} ms` : '—'}</b></div>
          <div><span>{t('link_fm_buffer')}</span><b>{l.fmDelayMs.toFixed(0)} ms</b></div>
          <div><span>{t('link_dab_buffer')}</span><b>{l.dabBufferMs.toFixed(0)} ms</b></div>
          <div title={t('link_gain_hint')}>
            <span>{t('link_gain')}</span>
            <b>{l.gainDb >= 0 ? '+' : ''}{l.gainDb.toFixed(1)} dB</b>
            <small>{t('link_gain_hint')}</small>
          </div>
          <div><span>{t('link_drift')}</span><b>{l.driftPpm.toFixed(1)} ppm</b></div>
          <div><span>{t('link_stalls')}</span><b>{l.stalls}</b></div>
          <div>
            <span>{t('link_polarity')}</span>
            <b>{t(fmMismatch(l.polarity, l.sidePolarity ?? 1, true))}</b>
          </div>
        </div>
      )}
      <div className="fm-panel">
        <div className="hint">{t('link_targets')}</div>
        <div className="fm-chips">
          {s.targets.map((x) => (
            <span key={x.pi} className="badge">PI {x.pi} · {x.source}</span>
          ))}
        </div>
        <div className="hint">{t('link_candidates')}</div>
        <table className="link-table">
          <tbody>
            {s.candidates.map((c) => (
              <tr key={c.khz} className={c.best ? 'ok' : ''}>
                <td>
                  {mhz(c.khz)} {c.best && <b>★ {t('link_best')}</b>}
                </td>
                <td className="dim">{c.sources.join(', ')}</td>
                <td>
                  {c.check?.measures.map((m) => (
                    <div key={m.antenna} className={m.score != null ? '' : 'dim'}>
                      {s.fm.antennas > 1 ? `A${m.antenna + 1}: ` : ''}
                      {m.level.toFixed(0)} dBf, CCI {m.cci.toFixed(0)}, ACI {m.aci.toFixed(0)}
                      {m.pi ? `, PI ${m.pi}` : ''}
                      {m.score != null ? ` → ${m.score.toFixed(0)}` : ''}
                    </div>
                  ))}
                </td>
                <td>{c.check?.result ?? ''}</td>
                <td className="dim">{c.check ? `${c.check.ago} s` : ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <details>
          <summary className="hint">
            {t('link_stations')} ({s.stations.length})
          </summary>
          <table className="link-table">
            <tbody>
              {s.stations.map((x) => (
                <tr key={x.khz} className={x.match ? 'ok' : ''}>
                  <td>{mhz(x.khz)}</td>
                  <td>{x.pi ? `PI ${x.pi}` : 'no RDS'}</td>
                  <td>{x.level.toFixed(0)} dBf</td>
                  <td className="dim">{x.ago < 3600 ? `${Math.round(x.ago / 60)} min` : `${Math.round(x.ago / 3600)} h`}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </details>
      </div>
      {m && (
        <div className="fm-panel">
          <div className="hint">
            {t('link_correlation')} · {t('link_confidence')} {m.confidence.toFixed(1)} · r = {m.correlation.toFixed(2)}
            {m.correlationHigh !== undefined && ` (${t('link_band_high')} ${m.correlationHigh.toFixed(2)}, ${t('link_band_low')} ${(m.correlationLow ?? 0).toFixed(2)}, L−P ${(m.correlationSide ?? 0).toFixed(2)})`} {m.accepted ? '✓' : '✗'}
          </div>
          <Plot data={m.overview} />
          <Plot data={m.curve} mark={Math.floor(m.curve.length / 2)} height={70} />
        </div>
      )}
      {s.history.length > 1 && (
        <div className="fm-panel">
          <div className="hint">{t('link_history')}</div>
          <Plot data={[]} points={s.history.map(([x, y, , ok]) => ({ x, y, ok }))} />
        </div>
      )}
      <div className="fm-panel">
        <div className="hint">{t('link_events')}</div>
        <div className="link-events">
          {s.events.map(([ms, text], i) => (
            <div key={i}>
              <span className="dim">{time(ms)}</span> {text}
            </div>
          ))}
        </div>
      </div>
      {fmOpen && (
        <div className="modal-back" onClick={() => setFmOpen(false)}>
          <div className="modal fm-modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <b>{t('fm_tuner')}</b>
              <span style={{ flex: 1 }} />
              <button className="icon-btn" onClick={() => setFmOpen(false)} aria-label="close">
                <X />
              </button>
            </div>
            <FmView />
          </div>
        </div>
      )}
    </div>
  )
}
