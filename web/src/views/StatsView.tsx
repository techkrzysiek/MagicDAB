import { useEffect, useRef, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader } from '../components/common'
import type { StatsPoint, StatsReceiver } from '../types'

type Tab = 'overview' | 'advanced'

/** Numbers in the language of the interface (a decimal comma in Polish). */
function useFormat() {
  const lang = useStore((s) => s.lang)
  const locale = lang === 'pl' ? 'pl-PL' : 'en-GB'
  const num = (v: number, digits = 1) => v.toLocaleString(locale, { minimumFractionDigits: digits, maximumFractionDigits: digits })
  const bytes = (n: number) =>
    n >= 1e9 ? `${num(n / 1e9)} GB` : n >= 1e6 ? `${num(n / 1e6)} MB` : n >= 1e3 ? `${num(n / 1e3, 0)} kB` : `${num(n, 0)} B`
  // (networks are counted in bits)
  const rate = (bytesPerSecond: number) => {
    const b = bytesPerSecond * 8
    return b >= 1e6 ? `${num(b / 1e6)} Mbit/s` : `${num(b / 1e3, 0)} kbit/s`
  }
  const pct = (v: number | null | undefined) => (v == null ? '' : `${num(v)}%`)
  return { num, bytes, rate, pct }
}

function since(secs: number, t: (k: string) => string) {
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  if (d) return `${d} ${t('stats_d')} ${h} ${t('stats_h')}`
  if (h) return `${h} ${t('stats_h')} ${m} ${t('stats_min')}`
  return `${m} ${t('stats_min')}`
}

function Tile({ label, value, sub, meter }: { label: string; value: string; sub?: string; meter?: number | null }) {
  // (the fill warns when the computer runs out of it)
  const level = meter == null ? '' : meter > 0.9 ? ' high' : meter > 0.7 ? ' mid' : ''
  return (
    <div className="stats-tile">
      <div className="stats-label">{label}</div>
      <div className="stats-value">{value}</div>
      {meter != null && (
        <div className={'stats-meter' + level} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(meter * 100)}>
          <span style={{ width: `${Math.min(100, Math.max(0, meter * 100))}%` }} />
        </div>
      )}
      {sub && <div className="stats-sub">{sub}</div>}
    </div>
  )
}

/** The last hour of one measure: a line with a crosshair and its value under the pointer. */
function Chart({ title, points, max, format }: { title: string; points: [number, number | null][]; max?: number; format: (v: number) => string }) {
  const t = useT()
  const box = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(600)
  const [hover, setHover] = useState<number | null>(null)
  useEffect(() => {
    const el = box.current
    if (!el) return
    const ro = new ResizeObserver(() => setWidth(el.clientWidth))
    ro.observe(el)
    setWidth(el.clientWidth)
    return () => ro.disconnect()
  }, [])
  const data = points.filter((p): p is [number, number] => p[1] != null)
  const H = 132
  const [left, right, top, bottom] = [52, 14, 10, 22]
  const now = Date.now() / 1000
  const t0 = now - 3600
  const peak = Math.max(...data.map((p) => p[1]), 0)
  // round tops: 1, 2, 5, 10, 20, 50...
  const nice = (v: number) => {
    const e = Math.pow(10, Math.floor(Math.log10(Math.max(v, 1e-9))))
    const f = v / e
    return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * e
  }
  const ymax = max ?? (peak > 0 ? nice(peak * 1.1) : 1)
  const x = (s: number) => left + ((s - t0) / 3600) * (width - left - right)
  const y = (v: number) => top + (1 - Math.min(v, ymax) / ymax) * (H - top - bottom)
  // a gap of over a minute (a restart) breaks the line
  let path = ''
  data.forEach((p, i) => {
    const gap = i === 0 || p[0] - data[i - 1][0] > 60
    path += `${gap ? 'M' : 'L'}${x(p[0]).toFixed(1)},${y(p[1]).toFixed(1)}`
  })
  const area = data.length > 1 ? `M${x(data[0][0]).toFixed(1)},${y(0)}${path.replace(/M/g, 'L')}L${x(data[data.length - 1][0]).toFixed(1)},${y(0)}Z` : ''
  const last = data[data.length - 1]
  const h = hover != null ? data[hover] : null
  const move = (e: React.PointerEvent) => {
    if (!data.length) return
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const px = e.clientX - r.left
    let best = 0
    data.forEach((p, i) => {
      if (Math.abs(x(p[0]) - px) < Math.abs(x(data[best][0]) - px)) best = i
    })
    setHover(best)
  }
  const time = (s: number) => new Date(s * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
  return (
    <figure className="stats-chart">
      <figcaption>{title}</figcaption>
      <div className="stats-plot" ref={box} onPointerMove={move} onPointerLeave={() => setHover(null)}>
        {data.length < 2 ? (
          <div className="stats-empty hint">{t('stats_waiting')}</div>
        ) : (
          <svg width={width} height={H} role="img" aria-label={title}>
            {[0, 0.5, 1].map((f) => (
              <g key={f}>
                <line className="grid" x1={left} x2={width - right} y1={y(ymax * f)} y2={y(ymax * f)} />
                <text className="axis" x={left - 6} y={y(ymax * f) + 4} textAnchor="end">
                  {format(ymax * f)}
                </text>
              </g>
            ))}
            {[
              [t0, '−60 min'],
              [t0 + 1800, '−30 min'],
              [now, t('stats_now')],
            ].map(([s, label], i) => (
              <text key={i} className="axis" x={x(s as number)} y={H - 6} textAnchor={i === 0 ? 'start' : i === 2 ? 'end' : 'middle'}>
                {label}
              </text>
            ))}
            <path className="area" d={area} />
            <path className="line" d={path} />
            {h && <line className="cross" x1={x(h[0])} x2={x(h[0])} y1={top} y2={H - bottom} />}
            {last && <circle className="dot" cx={x(last[0])} cy={y(last[1])} r={4} />}
            {h && <circle className="dot" cx={x(h[0])} cy={y(h[1])} r={4} />}
          </svg>
        )}
        {h && (
          <div className="stats-tip" style={{ left: Math.min(Math.max(x(h[0]), 70), width - 70) }}>
            <b>{format(h[1])}</b>
            <span>{time(h[0])}</span>
          </div>
        )}
      </div>
    </figure>
  )
}

function Overview() {
  const t = useT()
  const f = useFormat()
  const s = useStore((st) => st.stats)
  const history = useStore((st) => st.statsHistory)
  if (!s) return <p className="hint">{t('stats_waiting')}</p>
  const ram = s.system.ramTotal && s.system.ramUsed != null ? s.system.ramUsed / s.system.ramTotal : null
  return (
    <>
      <div className="stats-tiles">
        <Tile label={t('stats_listeners')} value={String(s.listeners.connected)} sub={t('stats_listening').replace('{n}', String(s.listeners.listening))} />
        <Tile label={t('stats_bandwidth')} value={f.rate(s.net.out)} sub={t('stats_sent_total').replace('{v}', f.bytes(s.net.sent))} />
        <Tile
          label={t('stats_cpu')}
          value={f.pct(s.system.cpu)}
          meter={s.system.cpu != null ? s.system.cpu / 100 : null}
          sub={t('stats_cpu_magicdab').replace('{v}', f.pct(s.process.cpu))}
        />
        <Tile
          label={t('stats_ram')}
          value={s.system.ramTotal ? (s.system.ramUsed != null ? `${f.bytes(s.system.ramUsed)} / ${f.bytes(s.system.ramTotal)}` : f.bytes(s.system.ramTotal)) : ''}
          meter={ram}
          sub={t('stats_ram_magicdab').replace('{v}', s.process.ram != null ? f.bytes(s.process.ram) : '')}
        />
      </div>
      <p className="stats-machine hint">
        {t('stats_uptime_line').replace('{a}', since(s.uptime, t))}
        {s.system.uptime != null && `, ${t('stats_uptime_pc').replace('{a}', since(s.system.uptime, t))}`}
        <br />
        {s.machine.cpuModel || t('stats_cpu')}, {t('stats_cores_n').replace('{n}', String(s.machine.cores))}, {s.machine.os}
      </p>
      <Chart title={t('stats_chart_cpu')} points={history.map((p: StatsPoint) => [p[0], p[1]])} max={100} format={(v) => `${f.num(v, 0)}%`} />
      <Chart title={t('stats_chart_net')} points={history.map((p: StatsPoint) => [p[0], p[3]])} format={(v) => f.rate(v)} />
    </>
  )
}

/** Threads by what they do. */
const THREADS: [RegExp, string][] = [
  [/^dab-receiver$/, 'th_receivers'],
  [/^radio$/, 'th_radio'],
  [/^tokio/, 'th_web'],
  [/^(magicdab|main|magicdab\.exe)$/, 'th_main'],
  [/^rtlsdr/, 'th_rtlsdr'],
  [/^rtltcp$/, 'th_rtltcp'],
  [/^rawfile$/, 'th_rawfile'],
  [/^eti-input$/, 'th_eti'],
  [/^fm-/, 'th_fm'],
  [/^tunnel/, 'th_tunnel'],
  [/^radio(dns|vis)/, 'th_radiodns'],
  [/^servermap$/, 'th_map'],
  [/^(access|asn)$/, 'th_access'],
  [/^window$/, 'th_window'],
  [/^log$/, 'th_log'],
]

function Receiver({ r }: { r: StatsReceiver }) {
  const t = useT()
  const f = useFormat()
  const row = (label: React.ReactNode, v: number | undefined, key: string) =>
    v != null && (
      <tr key={key}>
        <td>{label}</td>
        <td className="num">{f.pct(v)}</td>
      </tr>
    )
  const radio = r.input === 'rtlsdr' || r.input === 'rtltcp' || r.input === 'recording'
  return (
    <div className="stats-rx">
      <div className="stats-rx-head">
        <b>
          {t(r.slot === 0 ? 'stats_main_rx' : 'stats_side_rx')}, {r.channel || ''}
        </b>
        <span className="hint">{t('stats_input_' + r.input)}</span>
        <span className="stats-rx-cpu">{f.pct(r.cpu)}</span>
      </div>
      {r.cpu == null ? (
        <p className="hint">{t('stats_waiting')}</p>
      ) : (
        <table className="table stats-table">
          <tbody>
            {radio && row(t('stats_ofdm'), r.ofdm, 'ofdm')}
            {row(t('stats_fic'), r.fic, 'fic')}
            {(r.services ?? []).map((s) =>
              row(
                <>
                  {s.label}
                  {s.listeners > 0 && <span className="hint">, {t('stats_n_listen').replace('{n}', String(s.listeners))}</span>}
                  <div className="hint">
                    {t('stats_station_channel')} {f.pct(s.channel)}, {t('stats_station_audio')} {f.pct(s.audio)}, Opus {f.pct(s.opus)}
                  </div>
                </>,
                s.channel + s.audio + s.opus,
                String(s.sid),
              ),
            )}
            {row(t('stats_background'), r.background, 'bg')}
            {row(t('stats_data'), r.data, 'data')}
            {!!r.recording && row(t('stats_recording'), r.recording, 'rec')}
            {radio && row('TII', r.tii, 'tii')}
          </tbody>
        </table>
      )}
    </div>
  )
}

function Advanced() {
  const t = useT()
  const f = useFormat()
  const s = useStore((st) => st.stats)
  if (!s) return <p className="hint">{t('stats_waiting')}</p>
  const threads = new Map<string, { count: number; cpu: number }>()
  for (const th of s.threads ?? []) {
    const label = THREADS.find(([re]) => re.test(th.name))?.[1]
    const key = label ? t(label) : th.name
    const e = threads.get(key) ?? { count: 0, cpu: 0 }
    threads.set(key, { count: e.count + th.count, cpu: e.cpu + th.cpu })
  }
  const kv = (k: string, v: React.ReactNode) => (
    <tr key={k}>
      <td>{k}</td>
      <td className="num">{v}</td>
    </tr>
  )
  return (
    <>
      <section className="stats-card">
        <h3>{t('stats_receivers')}</h3>
        <p className="hint">{t('stats_hint_core')}</p>
        {(s.receivers ?? []).map((r) => (
          <Receiver key={r.slot} r={r} />
        ))}
      </section>
      <section className="stats-card">
        <h3>{t('stats_threads')}</h3>
        <table className="table stats-table">
          <tbody>
            {[...threads.entries()]
              .filter(([, v]) => v.cpu >= 0.1)
              .sort((a, b) => b[1].cpu - a[1].cpu)
              .map(([name, v]) =>
                kv(name + (v.count > 1 ? ` (${v.count})` : ''), f.pct(v.cpu)),
              )}
            {/* (the quiet ones together) */}
            {kv(
              t('stats_threads_rest'),
              f.pct([...threads.values()].filter((v) => v.cpu < 0.1).reduce((sum, v) => sum + v.cpu, 0)),
            )}
          </tbody>
        </table>
      </section>
      <section className="stats-card">
        <h3>{t('stats_network')}</h3>
        <table className="table stats-table">
          <tbody>
            {kv(t('stats_out_local'), f.rate(s.net.local ?? 0))}
            {kv(t('stats_out_tunnel'), f.rate(s.net.tunnel ?? 0))}
            {(s.net.inputs ?? []).map((i) => kv(`${t('stats_input_from')} ${i.name}`, f.rate(i.rate)))}
            {kv(t('stats_pcm'), String(s.listeners.pcm ?? 0))}
          </tbody>
        </table>
      </section>
      <section className="stats-card">
        <h3>{t('stats_system')}</h3>
        <table className="table stats-table">
          <tbody>
            {kv(t('stats_os'), s.machine.os)}
            {kv(t('stats_cpu'), s.machine.cpuModel || '')}
            {kv(t('stats_cores'), String(s.machine.cores))}
            {kv(t('stats_threads_n'), String(s.process.threads))}
            {kv(t('stats_uptime'), since(s.uptime, t))}
            {s.system.uptime != null && kv(t('stats_uptime_pc_k'), since(s.system.uptime, t))}
            {kv(t('stats_version'), s.version)}
          </tbody>
        </table>
      </section>
    </>
  )
}

export default function StatsView() {
  const t = useT()
  const [tab, setTab] = useState<Tab>('overview')
  return (
    <>
      <ViewHeader />
      <div className="view-body stats-view">
        <div className="seg stats-tabs" role="tablist">
          {(['overview', 'advanced'] as Tab[]).map((x) => (
            <button key={x} role="tab" aria-selected={tab === x} className={tab === x ? 'on' : ''} onClick={() => setTab(x)}>
              {t('stats_' + x)}
            </button>
          ))}
        </div>
        {tab === 'overview' ? <Overview /> : <Advanced />}
      </div>
    </>
  )
}
