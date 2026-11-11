import { useEffect, useMemo, useRef, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader } from '../components/common'
import { Kv, useRates } from './InfoView'

const GRID = '#2e3046'
const TEXT = '#c3c6dc'

function niceStep(range: number, target: number) {
  const raw = range / target
  const p = Math.pow(10, Math.floor(Math.log10(raw)))
  const m = raw / p
  return (m < 1.5 ? 1 : m < 3 ? 2 : m < 7 ? 5 : 10) * p
}

/** Samples per microsecond (2.048 MS/s) and km per microsecond of delay. */
const SAMPLES_US = 2.048
const KM_US = 0.29979

/** Modulation error ratio of normalised DQPSK points (ideal: the diagonals). */
function mer(pts: Float32Array[]): number | null {
  let n = 0
  let err = 0
  for (const p of pts) {
    for (let i = 0; i + 1 < p.length; i += 2) {
      const er = p[i] - Math.SQRT1_2 * (p[i] < 0 ? -1 : 1)
      const ei = p[i + 1] - Math.SQRT1_2 * (p[i + 1] < 0 ? -1 : 1)
      err += er * er + ei * ei
      n++
    }
  }
  return n > 0 && err > 0 ? 10 * Math.log10(n / err) : null
}

interface Path {
  delayUs: number
  db: number
  share: number
  /** outside the guard interval of the FFT window: interference between symbols */
  isi: boolean
}

/**
 * Signal paths of a single frequency network in the power delay profile: peaks
 * above the noise, their delay and level against the strongest one and their
 * share of the power of all paths. Only down to 20 dB below the strongest:
 * weaker peaks are mostly the response's own side lobes, the tuner's ripple
 * and nearby reflections, not other transmitters (a multiplex with no SFN
 * showed three of them at -21 to -27 dB).
 */
function sfnPaths(imp: Float32Array, noise: number, guard: number): Path[] {
  const k = imp.length
  let max = -Infinity
  let jm = 0
  for (let j = 0; j < k; j++) {
    if (imp[j] > max) {
      max = imp[j]
      jm = j
    }
  }
  const floor = Math.max(noise + 10, max - 20)
  const peaks: { j: number; db: number }[] = []
  for (let j = 1; j < k - 1; j++) {
    const v = imp[j]
    if (v < floor || v < imp[j - 1] || v < imp[j + 1]) continue
    // one peak per path: the strongest within 4 taps
    const near = peaks.find((p) => Math.abs(p.j - j) <= 4)
    if (near) {
      if (v > near.db) Object.assign(near, { j, db: v })
    } else peaks.push({ j, db: v })
  }
  const power = peaks.map((p) => {
    let s = 0
    for (let d = -2; d <= 2; d++) if (p.j + d >= 0 && p.j + d < k) s += Math.pow(10, imp[p.j + d] / 10)
    return s
  })
  const total = power.reduce((a, b) => a + b, 0)
  return peaks
    .map((p, i) => ({
      delayUs: (p.j - jm) / SAMPLES_US,
      db: p.db - max,
      share: total > 0 ? power[i] / total : 0,
      // (a few samples outside interfere too little to matter)
      isi: p.j - k / 2 < -16 || p.j - k / 2 > guard + 16,
    }))
    .sort((a, b) => b.share - a.share)
    .slice(0, 6)
    .sort((a, b) => a.delayUs - b.delayUs)
}

function useCanvas(draw: (g: CanvasRenderingContext2D, w: number, h: number, dpr: number) => void, deps: unknown[]) {
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const c = ref.current
    if (!c) return
    const render = () => {
      const dpr = window.devicePixelRatio || 1
      const w = (c.width = Math.max(1, c.clientWidth * dpr))
      const h = (c.height = Math.max(1, c.clientHeight * dpr))
      const g = c.getContext('2d')!
      draw(g, w, h, dpr)
    }
    render()
    const ro = new ResizeObserver(render)
    ro.observe(c)
    return () => ro.disconnect()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)
  return ref
}

export default function SignalView() {
  const t = useT()
  const spectrum = useStore((s) => s.spectrum)
  const signal = useStore((s) => s.signal)
  const state = useStore((s) => s.state)
  const history = useStore((s) => s.snrHistory)
  const [showNull, setShowNull] = useState(true)
  const [showWaterfall, setShowWaterfall] = useState(false)
  const avg = useRef<Float32Array | null>(null)
  const avgNull = useRef<Float32Array | null>(null)
  const water = useRef<Float32Array[]>([])
  // the points of the last few frames together
  const cons = useRef<Float32Array[]>([])
  const data = useStore((s) => (s.selected ? s.services[s.selected.sid] : undefined))
  const rates = useRates(data?.stats)

  // smooth the spectra a little
  if (spectrum && (!avg.current || avg.current.length !== spectrum.spectrum.length)) {
    avg.current = Float32Array.from(spectrum.spectrum)
    avgNull.current = Float32Array.from(spectrum.nullSpectrum)
  }
  useEffect(() => {
    if (!spectrum || !avg.current || !avgNull.current) return
    for (let i = 0; i < spectrum.spectrum.length; i++) {
      avg.current[i] = 0.6 * avg.current[i] + 0.4 * spectrum.spectrum[i]
      avgNull.current[i] = 0.6 * avgNull.current[i] + 0.4 * spectrum.nullSpectrum[i]
    }
    water.current = [Float32Array.from(spectrum.spectrum), ...water.current].slice(0, 200)
    if (spectrum.constellation?.length) cons.current = [Float32Array.from(spectrum.constellation), ...cons.current].slice(0, 3)
    else cons.current = []
  }, [spectrum])
  const merDb = useMemo(() => (spectrum ? mer(cons.current) : null), [spectrum])
  const impulse = spectrum?.impulse?.length ? spectrum.impulse : null
  const paths = useMemo(
    () => (impulse && spectrum ? sfnPaths(impulse, spectrum.noise ?? -200, spectrum.guard ?? 504) : []),
    [impulse, spectrum],
  )

  const consRef = useCanvas(
    (g, w, h, dpr) => {
      g.fillStyle = '#12131f'
      g.fillRect(0, 0, w, h)
      const r = (Math.min(w, h) / 2) * 0.92
      const cx = w / 2
      const cy = h / 2
      const scale = r / 1.6
      g.strokeStyle = GRID
      g.lineWidth = 1
      g.beginPath()
      g.moveTo(cx - r, cy)
      g.lineTo(cx + r, cy)
      g.moveTo(cx, cy - r)
      g.lineTo(cx, cy + r)
      g.stroke()
      g.beginPath()
      g.arc(cx, cy, scale, 0, 2 * Math.PI)
      g.stroke()
      const pts = cons.current
      if (!pts.length) {
        g.fillStyle = TEXT
        g.font = `${13 * dpr}px sans-serif`
        g.fillText(t('no_signal'), 10 * dpr, 20 * dpr)
        return
      }
      g.fillStyle = 'rgba(179,157,255,0.45)'
      const d = 1.5 * dpr
      for (const p of pts) {
        for (let i = 0; i + 1 < p.length; i += 2) g.fillRect(cx + p[i] * scale - d / 2, cy - p[i + 1] * scale - d / 2, d, d)
      }
      // the ideal points
      g.strokeStyle = '#ef5d8a'
      for (const [x, y] of [[1, 1], [1, -1], [-1, 1], [-1, -1]]) {
        g.beginPath()
        g.arc(cx + x * Math.SQRT1_2 * scale, cy - y * Math.SQRT1_2 * scale, 4 * dpr, 0, 2 * Math.PI)
        g.stroke()
      }
    },
    [spectrum, t],
  )

  const irRef = useCanvas(
    (g, w, h, dpr) => {
      g.fillStyle = '#12131f'
      g.fillRect(0, 0, w, h)
      const left = 40 * dpr
      const right = 10 * dpr
      const top = 10 * dpr
      const bottom = 28 * dpr
      const pw = w - left - right
      const ph = h - top - bottom
      if (!impulse || !spectrum) {
        g.fillStyle = TEXT
        g.font = `${13 * dpr}px sans-serif`
        g.fillText(t('no_signal'), left + 10 * dpr, top + 20 * dpr)
        return
      }
      const k = impulse.length
      let max = -Infinity
      let jm = 0
      for (let j = 0; j < k; j++) {
        if (impulse[j] > max) {
          max = impulse[j]
          jm = j
        }
      }
      // microseconds around the strongest path, dB below it
      const x0 = -200
      const x1 = 400
      const yMin = -40
      const x = (us: number) => left + ((us - x0) / (x1 - x0)) * pw
      const y = (db: number) => top + (Math.min(0, Math.max(yMin, db)) / yMin) * ph
      // guard interval of the FFT window (no interference for paths inside)
      const g0 = (k / 2 - jm) / SAMPLES_US
      const g1 = (k / 2 + (spectrum.guard ?? 504) - jm) / SAMPLES_US
      g.fillStyle = 'rgba(34,179,166,0.14)'
      g.fillRect(x(Math.max(x0, g0)), top, Math.max(0, x(Math.min(x1, g1)) - x(Math.max(x0, g0))), ph)
      g.strokeStyle = GRID
      g.fillStyle = TEXT
      g.font = `${11 * dpr}px sans-serif`
      for (let v = 0; v >= yMin; v -= 10) {
        g.beginPath()
        g.moveTo(left, y(v))
        g.lineTo(left + pw, y(v))
        g.stroke()
        g.fillText(`${v}`, 8 * dpr, y(v) + 4 * dpr)
      }
      for (let us = x0; us <= x1; us += 100) {
        g.beginPath()
        g.moveTo(x(us), top)
        g.lineTo(x(us), top + ph)
        g.stroke()
        // (the unit in place of the last number)
        g.fillText(us < x1 ? `${us}` : 'µs', x(us) - 10 * dpr, top + ph + 16 * dpr)
      }
      // noise of one tap
      if (spectrum.noise !== undefined) {
        g.setLineDash([3 * dpr, 3 * dpr])
        g.strokeStyle = '#ffb454'
        g.beginPath()
        g.moveTo(left, y(spectrum.noise - max))
        g.lineTo(left + pw, y(spectrum.noise - max))
        g.stroke()
        g.setLineDash([])
      }
      g.strokeStyle = '#b39dff'
      g.lineWidth = 1.2 * dpr
      g.beginPath()
      let started = false
      for (let j = 0; j < k; j++) {
        const us = (j - jm) / SAMPLES_US
        if (us < x0 || us > x1) continue
        const yy = y(impulse[j] - max)
        if (!started) g.moveTo(x(us), yy)
        else g.lineTo(x(us), yy)
        started = true
      }
      g.stroke()
      g.strokeStyle = '#9aa0a6'
      g.lineWidth = 1
      g.strokeRect(left, top, pw, ph)
    },
    [spectrum, t],
  )
  const offset = signal?.synced ? signal.freqOffset : null
  const ber = (v: number | undefined) => (v === undefined ? '' : v.toExponential(2))

  const specRef = useCanvas(
    (g, w, h, dpr) => {
      g.fillStyle = '#12131f'
      g.fillRect(0, 0, w, h)
      const left = 56 * dpr
      const right = 14 * dpr
      const top = 12 * dpr
      const bottom = 34 * dpr
      const pw = w - left - right
      const ph = (showWaterfall ? (h - top - bottom) * 0.55 : h - top - bottom)
      const data = avg.current
      if (!data || !spectrum) {
        g.fillStyle = TEXT
        g.font = `${13 * dpr}px sans-serif`
        g.fillText(t('no_signal'), left + 10 * dpr, top + 20 * dpr)
        return
      }
      const n = data.length
      let max = -1e9
      let min = 1e9
      for (let i = 0; i < n; i++) {
        max = Math.max(max, data[i])
        min = Math.min(min, data[i])
      }
      const nd = avgNull.current
      if (nd && showNull && spectrum.hasNull !== false) for (let i = 0; i < n; i++) min = Math.min(min, nd[i])
      const yMax = Math.ceil((max + 5) / 10) * 10
      const yMin = Math.max(yMax - 90, Math.floor((min - 5) / 10) * 10)
      const fc = spectrum.frequency / 1000
      const span = 2.048
      const x = (i: number) => left + (i / (n - 1)) * pw
      const y = (v: number) => top + ((yMax - v) / (yMax - yMin)) * ph
      // grid
      g.strokeStyle = GRID
      g.lineWidth = 1
      g.fillStyle = TEXT
      g.font = `${11 * dpr}px sans-serif`
      const ys = niceStep(yMax - yMin, 6)
      for (let v = Math.ceil(yMin / ys) * ys; v <= yMax; v += ys) {
        g.beginPath()
        g.moveTo(left, y(v))
        g.lineTo(left + pw, y(v))
        g.stroke()
        g.fillText(`${v.toFixed(0)}`, 8 * dpr, y(v) + 4 * dpr)
      }
      const fs = 0.2
      for (let f = Math.ceil((fc - span / 2) / fs) * fs; f <= fc + span / 2; f += fs) {
        const xi = left + ((f - (fc - span / 2)) / span) * pw
        g.beginPath()
        g.moveTo(xi, top)
        g.lineTo(xi, top + ph)
        g.stroke()
        g.fillText(f.toFixed(1), xi - 12 * dpr, top + ph + 16 * dpr)
      }
      g.fillText('dB', 8 * dpr, top + 2 * dpr)
      g.fillText(t('frequency') + ' [MHz]', left + pw / 2 - 40 * dpr, h - 4 * dpr)
      // DAB block edges (±768 kHz)
      g.setLineDash([3 * dpr, 3 * dpr])
      g.strokeStyle = '#e0e0e0'
      for (const off of [-0.768, 0.768]) {
        const xi = left + ((off + span / 2) / span) * pw
        g.beginPath()
        g.moveTo(xi, top)
        g.lineTo(xi, top + ph)
        g.stroke()
      }
      g.setLineDash([])
      // centre line
      g.strokeStyle = '#ef5d8a'
      g.beginPath()
      g.moveTo(left + pw / 2, top)
      g.lineTo(left + pw / 2, top + ph)
      g.stroke()
      // null symbol
      if (nd && showNull && spectrum.hasNull !== false) {
        g.strokeStyle = '#ffb454'
        g.lineWidth = 1 * dpr
        g.beginPath()
        for (let i = 0; i < n; i++) {
          const yy = y(Math.max(yMin, nd[i]))
          if (i === 0) g.moveTo(x(i), yy)
          else g.lineTo(x(i), yy)
        }
        g.stroke()
      }
      // spectrum
      g.strokeStyle = '#b39dff'
      g.lineWidth = 1.2 * dpr
      g.beginPath()
      for (let i = 0; i < n; i++) {
        const yy = y(Math.max(yMin, data[i]))
        if (i === 0) g.moveTo(x(i), yy)
        else g.lineTo(x(i), yy)
      }
      g.stroke()
      g.strokeStyle = '#9aa0a6'
      g.strokeRect(left, top, pw, ph)
      // waterfall
      if (showWaterfall) {
        const wt = top + ph + 24 * dpr
        const wh = h - wt - bottom + 10 * dpr
        const rows = water.current
        const img = g.createImageData(Math.max(1, Math.floor(pw)), Math.max(1, Math.floor(wh)))
        for (let r = 0; r < img.height; r++) {
          const row = rows[Math.floor((r / img.height) * rows.length)]
          if (!row) continue
          for (let c = 0; c < img.width; c++) {
            const v = row[Math.floor((c / img.width) * n)]
            const z = Math.min(1, Math.max(0, (v - yMin) / (yMax - yMin)))
            const p = (r * img.width + c) * 4
            img.data[p] = Math.floor(255 * Math.min(1, z * 2 - 0.5))
            img.data[p + 1] = Math.floor(255 * Math.min(1, z * 1.5))
            img.data[p + 2] = Math.floor(255 * Math.min(1, 0.4 + z))
            img.data[p + 3] = 255
          }
        }
        g.putImageData(img, left, wt)
      }
    },
    [spectrum, showNull, showWaterfall, t],
  )

  const snrRef = useCanvas(
    (g, w, h, dpr) => {
      g.fillStyle = '#12131f'
      g.fillRect(0, 0, w, h)
      const left = 46 * dpr
      const right = 10 * dpr
      const top = 10 * dpr
      const bottom = 28 * dpr
      const pw = w - left - right
      const ph = h - top - bottom
      const now = Date.now() / 1000
      const window = 120
      const pts = history.filter((p) => now - p.t <= window)
      const yMax = Math.max(30, Math.ceil(Math.max(0, ...pts.map((p) => p.snr)) / 10) * 10)
      g.strokeStyle = GRID
      g.fillStyle = TEXT
      g.font = `${11 * dpr}px sans-serif`
      for (let v = 0; v <= yMax; v += 10) {
        const yy = top + ph - (v / yMax) * ph
        g.beginPath()
        g.moveTo(left, yy)
        g.lineTo(left + pw, yy)
        g.stroke()
        g.fillText(`${v}`, 14 * dpr, yy + 4 * dpr)
      }
      g.save()
      g.translate(12 * dpr, top + ph / 2 + 20 * dpr)
      g.rotate(-Math.PI / 2)
      g.fillText('SNR [dB]', 0, 0)
      g.restore()
      if (pts.length > 1) {
        g.beginPath()
        g.moveTo(left + pw - ((now - pts[0].t) / window) * pw, top + ph)
        for (const p of pts) g.lineTo(left + pw - ((now - p.t) / window) * pw, top + ph - (Math.max(0, p.snr) / yMax) * ph)
        g.lineTo(left + pw - ((now - pts[pts.length - 1].t) / window) * pw, top + ph)
        g.closePath()
        g.fillStyle = 'rgba(124,92,255,0.45)'
        g.fill()
        g.strokeStyle = '#a68bff'
        g.lineWidth = 1.5 * dpr
        g.stroke()
      }
      g.strokeStyle = '#9aa0a6'
      g.strokeRect(left, top, pw, ph)
      g.fillStyle = TEXT
      g.fillText(`-${window} s`, left, h - 8 * dpr)
      g.fillText(t('live'), left + pw - 24 * dpr, h - 8 * dpr)
    },
    [history, t],
  )

  return (
    <>
      <ViewHeader />
      <div className="signal-view">
        <div className="signal-top">
          <canvas ref={specRef} />
          <div className="plot-legend">
            <label>
              <input type="checkbox" checked={showNull} onChange={(e) => setShowNull(e.target.checked)} /> {t('null_symbol')}
            </label>
            <label>
              <input type="checkbox" checked={showWaterfall} onChange={(e) => setShowWaterfall(e.target.checked)} /> {t('waterfall')}
            </label>
          </div>
        </div>
        <div className="signal-bottom">
          <div className="snrplot">
            <canvas ref={snrRef} />
          </div>
          <div className="bigsnr">
            <small>SNR</small>
            {signal?.synced ? `${signal.snr.toFixed(1)} dB` : ''}
          </div>
        </div>
        <div className="signal-extra">
          <div className="card signal-card">
            <div className="card-title">
              {t('constellation')}
              <span className="hint" title={t('mer_hint')}>
                , MER {merDb !== null ? `${merDb.toFixed(1)} dB` : ''}
              </span>
            </div>
            <div className="cons-plot">
              <canvas ref={consRef} />
            </div>
          </div>
          <div className="card signal-card wide">
            <div className="card-title" title={t('impulse_hint')}>
              {t('impulse_response')}
            </div>
            <div className="ir-plot">
              <canvas ref={irRef} />
            </div>
            <div className="card-title">{t('sfn_paths')}</div>
            {/* (as high for one path as for six: nothing below it moves) */}
            <div className="sfn-box">
              {paths.length > 1 ? (
                <table className="sfn-table">
                  <thead>
                    <tr>
                      <th>{t('sfn_delay')}</th>
                      <th>{t('sfn_distance')}</th>
                      <th>{t('sfn_level')}</th>
                      <th>{t('sfn_share')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {paths.map((p, i) => (
                      <tr key={i} className={p.isi ? 'isi' : ''} title={p.isi ? t('sfn_isi') : undefined}>
                        <td>{p.delayUs >= 0 ? '+' : ''}{p.delayUs.toFixed(1)} µs</td>
                        <td>{(p.delayUs * KM_US).toFixed(1)} km</td>
                        <td>{p.db.toFixed(1)} dB</td>
                        <td>
                          <span className="sfn-bar" style={{ width: `${Math.round(p.share * 60)}px` }} />
                          {(p.share * 100).toFixed(p.share < 0.1 ? 1 : 0)} %
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <div className="hint">{impulse ? t('sfn_none') : ''}</div>
              )}
            </div>
          </div>
          <div className="card signal-card">
            <Kv
              title={t('error_correction')}
              rows={[
                [t('fic_ber'), signal?.synced ? ber(signal.ficBer) : ''],
                [t('fib_error_rate'), signal?.synced ? ber(signal.fibErrorRate) : ''],
                [t('rs_ber'), data?.stats ? rates.rsBer.toExponential(2) : ''],
                [t('rs_uncorrectable'), data?.stats?.rsUncorrectable ?? ''],
                [t('audio_error_rate'), data?.stats ? rates.auErrRate.toExponential(2) : ''],
              ]}
            />
            <div className="card-title">{t('freq_offset')}</div>
            <div className="big-value">
              {offset !== null ? `${offset >= 0 ? '+' : ''}${offset.toFixed(1)} Hz` : ''}
              {offset !== null && state?.frequency ? (
                <small>
                  {' '}
                  {((offset * 1000) / state.frequency).toFixed(2)} ppm {t('freq_offset_ppm')}
                </small>
              ) : null}
            </div>
          </div>
        </div>
      </div>
      <div className="signal-info">
        <span>
          <span className="k">{t('rf_level')}</span>
          {signal?.rfLevel !== null && signal?.rfLevel !== undefined ? `${signal.rfLevel.toFixed(1)} dBm` : ''}
        </span>
        <span>
          <span className="k">{t('agc_gain')}</span>
          {signal?.gain !== null && signal?.gain !== undefined ? `${signal.gain.toFixed(1)} dB` : ''}
        </span>
        <span style={{ flex: 1 }} />
        {signal?.duration ? (
          <span>
            <span className="k">{t('file_position')}</span>
            {(signal.position ?? 0).toFixed(0)} / {signal.duration.toFixed(0)} s
          </span>
        ) : null}
        <span>
          <span className="k">{t('frequency')}</span>
          {state ? `${state.frequency} kHz` : ''}
        </span>
        <span>
          <span className="k">{t('freq_offset')}</span>
          {signal?.synced ? `${signal.freqOffset.toFixed(1)} Hz` : ''}
        </span>
      </div>
    </>
  )
}
