import { Fragment, useEffect, useRef, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader, CacheTag, Logo } from '../components/common'
import { downloadText, fmMhz, formatDuration, hex, mhz } from '../util'
import { send } from '../ws'
import type { AudioStats, Ensemble, Service, Subchannel } from '../types'

const SUBCH_COLORS = ['#7c5cff', '#22b3a6', '#f5a524', '#ef5d8a', '#4a9df8', '#9bc53d', '#c06cdb', '#f08a4b']
/** Capacity units of a multiplex (DAB transmission mode I). */
const CUS = 864

export function useRates(stats?: AudioStats) {
  const prev = useRef<{ s: AudioStats; t: number } | null>(null)
  const [rates, setRates] = useState({ audio: 0, pad: 0, auErrRate: 0, rsBer: 0 })
  useEffect(() => {
    if (!stats) return
    const now = Date.now()
    const p = prev.current
    if (p && stats.frames > p.s.frames) {
      // 10 frames of 96 ms between two statistics messages
      const dt = ((stats.frames - p.s.frames) * 0.024) || 1
      const audio = ((stats.audioBytes - p.s.audioBytes) * 8) / dt / 1000
      const pad = ((stats.padBytes - p.s.padBytes) * 8) / dt / 1000
      const au = stats.auOk + stats.auErrors - (p.s.auOk + p.s.auErrors)
      const auErr = stats.auErrors - p.s.auErrors
      const cw = stats.rsCodewords - p.s.rsCodewords
      const bits = stats.rsCorrectedBits - p.s.rsCorrectedBits
      setRates({ audio, pad, auErrRate: au > 0 ? auErr / au : 0, rsBer: cw > 0 ? bits / (cw * 120 * 8) : 0 })
    }
    prev.current = { s: stats, t: now }
  }, [stats])
  return rates
}

export function Kv({ rows, title }: { rows: [string, React.ReactNode][]; title?: string }) {
  return (
    <>
    {title && <div className="card-title">{title}</div>}
    <div className="kv">
      {rows.map(([k, v]) => (
        <div key={k} style={{ display: 'contents' }}>
          <span className="k">{k}</span>
          <span className="v">{v}</span>
        </div>
      ))}
    </div>
    </>
  )
}

/** One line per service component, for spreadsheets. */
function ensembleCsv(ens: Ensemble) {
  const head = [
    'channel', 'frequency_khz', 'eid', 'ecc', 'ensemble', 'sid', 'service', 'short_label', 'pty', 'language', 'country',
    'component', 'type', 'subchannel', 'start_cu', 'cu', 'protection', 'bitrate_kbps', 'codec', 'user_apps',
  ].join(';')
  const rows = (ens.services ?? []).flatMap((s) =>
    s.components.map((c) => {
      const sc = ens.subchannels?.find((x) => x.id === c.subch)
      return [
        ens.channel ?? '', ens.frequency, ens.eidHex ?? '', ens.eccHex ?? '', ens.label ?? '', s.sidHex, s.label, s.short,
        s.ptyName ?? '', c.language ?? s.language ?? '', s.countryName ?? '', c.label ?? s.label, c.type, sc?.id ?? '',
        sc?.start ?? '', sc?.size ?? '', sc?.protection ?? c.protection ?? '', c.bitrate ?? '',
        c.tmid === 0 ? (c.ctype === 63 ? 'DAB+' : 'DAB') : '', c.userApps.map((u) => u.name).join(', '),
      ].join(';')
    }),
  )
  return [head, ...rows].join('\n')
}

const share = (cu: number | undefined) => `${(((cu ?? 0) / CUS) * 100).toFixed(1)} %`
const appName = (name: string) => (name === 'SlideShow' ? 'SLS' : name)

/** Local time offset of the ensemble (FIG 0/9, half hours). */
function utcOffset(lto: number) {
  const m = Math.abs(lto) * 30
  return `UTC${lto < 0 ? '−' : '+'}${Math.floor(m / 60)}${m % 60 ? ':' + String(m % 60).padStart(2, '0') : ''}`
}

function Metric({ label, value, tone }: { label: string; value: React.ReactNode; tone?: string }) {
  return (
    <div className="metric" style={tone ? { borderColor: tone } : undefined}>
      <span>{label}</span>
      <b>{value}</b>
    </div>
  )
}

function snrTone(snr: number) {
  return snr >= 15 ? 'var(--sig-good)' : snr >= 10 ? 'var(--sig-mid)' : snr >= 6 ? 'var(--sig-low)' : 'var(--sig-none)'
}

/** Who the multiplex is, and how it is received. */
function EnsembleHead({ ens }: { ens: Ensemble }) {
  const t = useT()
  const signal = useStore((s) => s.signal)
  const synced = !!signal?.synced
  return (
    <div className="card ens-head">
      <div>
        <div className="ens-name">
          {ens.label}
          {ens.short && ens.short !== ens.label && <span className="ens-short">{ens.short}</span>}
        </div>
        {ens.long && <div className="hint">{ens.long}</div>}
      </div>
      <div className="ens-facts">
        <span>
          <b>{ens.channel}</b> {mhz(ens.frequency)} MHz
        </span>
        <span>
          EId <b className="mono">{ens.eidHex}</b>
        </span>
        {ens.eccHex && (
          <span>
            ECC <b className="mono">{ens.eccHex}</b>
            {ens.countryName ? ` ${ens.countryName}` : ''}
          </span>
        )}
        {ens.lto !== null && ens.lto !== undefined && <span>{utcOffset(ens.lto)}</span>}
        <span>
          <b>{ens.services?.length ?? 0}</b> {t('ens_services_n')}
        </span>
        {ens.alarm && <span>{t('ens_alarm')}</span>}
      </div>
      <div className="metrics">
        <Metric label={t('snr')} value={synced ? `${signal!.snr.toFixed(1)} dB` : '–'} tone={synced ? snrTone(signal!.snr) : undefined} />
        <Metric label={t('freq_offset')} value={synced ? `${signal!.freqOffset.toFixed(1)} Hz` : '–'} />
        {signal?.gain !== null && signal?.gain !== undefined && <Metric label={t('agc_gain')} value={`${signal.gain.toFixed(1)} dB`} />}
        {signal?.rfLevel !== null && signal?.rfLevel !== undefined && <Metric label={t('rf_level')} value={`${signal.rfLevel.toFixed(1)} dBm`} />}
        <Metric label={t('fib_error_rate')} value={(signal?.fibErrorRate ?? 0).toExponential(1)} />
      </div>
    </div>
  )
}

/** The station being listened to: what its bitrate carries and how well it arrives. */
function NowCard({ svc }: { svc?: Service }) {
  const t = useT()
  const data = useStore((s) => (svc ? s.services[svc.sid] : undefined))
  const rates = useRates(data?.stats)
  const st = data?.stats
  if (!svc) {
    return (
      <div className="card ens-now">
        <div className="card-title">{t('ens_now')}</div>
        <div className="hint">{t('click_to_play')}</div>
      </div>
    )
  }
  const total = svc.bitrate ?? 0
  const audio = Math.max(0, rates.audio - rates.pad)
  const pad = rates.pad
  const overhead = rates.audio ? Math.max(0, total - rates.audio) : 0
  const width = (kbps: number) => `${total ? Math.min(100, (kbps / total) * 100) : 0}%`
  return (
    <div className="card ens-now">
      <div className="card-title">{t('ens_now')}</div>
      <div className="now-svc">
        <Logo label={svc.label} sid={svc.sid} small />
        <div>
          <b>{svc.label}</b>
          <div className="hint">
            {svc.codec ?? t('data')} · {total} kbps · {svc.protection ?? ''}
          </div>
        </div>
      </div>
      <div className="rate-bar" title={t('ens_rate_hint')}>
        <i className="audio" style={{ width: width(audio) }} />
        <i className="pad" style={{ width: width(pad) }} />
        <i className="over" style={{ width: width(overhead) }} />
      </div>
      <div className="rate-legend">
        <span>
          <i className="audio" />
          {t('audio')} {audio.toFixed(1)} kbps
        </span>
        <span>
          <i className="pad" />
          PAD {pad.toFixed(1)} kbps
        </span>
        <span>
          <i className="over" />
          {t('ens_overhead')} {overhead.toFixed(1)} kbps
        </span>
      </div>
      <div className="metrics">
        <Metric label={t('rs_ber')} value={rates.rsBer.toExponential(1)} />
        <Metric label={t('rs_uncorrectable')} value={st?.rsUncorrectable ?? 0} tone={st?.rsUncorrectable ? 'var(--sig-low)' : undefined} />
        <Metric label={t('ens_audio_errors')} value={`${st?.auErrors ?? 0} · ${rates.auErrRate.toExponential(1)}`} tone={st?.auErrors ? 'var(--sig-low)' : undefined} />
      </div>
    </div>
  )
}

/** The 864 capacity units with the sub-channels where they lie. */
function CuMap({ ens, sel, color, onPick }: { ens: Ensemble; sel: number | null; color: (id: number) => string; onPick: (id: number) => void }) {
  const t = useT()
  return (
    <div className="card cu-map">
      <div className="cu-head">
        <span className="card-title">{t('ens_capacity')}</span>
        <span className="hint">
          {t('ens_used')} <b>{ens.usedCu} CU</b> ({share(ens.usedCu)}) · {t('audio')} {ens.audioCu} · {t('data')} {ens.dataCu} · {t('ens_free')} {ens.freeCu}
        </span>
      </div>
      <div className="cu-track">
        {(ens.subchannels ?? []).map((x) => (
          <button
            key={x.id}
            className={'cu-seg' + (x.audio ? '' : ' data') + (x.id === sel ? ' sel' : '')}
            style={{ left: `${(x.start / CUS) * 100}%`, width: `${(x.size / CUS) * 100}%`, background: color(x.id) }}
            title={`${t('subchannel')} ${x.id}: ${x.services.join(', ')} · CU ${x.start}–${x.start + x.size - 1} · ${x.bitrate} kbps · ${x.protection}`}
            onClick={() => onPick(x.id)}
          >
            <span>{x.services.join(', ')}</span>
          </button>
        ))}
      </div>
      <div className="cu-scale">
        {[0, 216, 432, 648, CUS].map((v) => (
          <span key={v} style={{ left: `${(v / CUS) * 100}%` }}>
            {v}
          </span>
        ))}
      </div>
    </div>
  )
}

/** Everything known of one service: its components, their sub-channels and applications, its links. */
function ServiceDetails({ s, ens, sub }: { s: Service; ens: Ensemble; sub: (id: number | null) => Subchannel | undefined }) {
  const t = useT()
  const facts: [string, string][] = [
    ['ECC', hex(s.ecc ?? ens.ecc ?? 0, 2) + (s.countryName ? ` ${s.countryName}` : '')],
    [t('pty'), s.ptyName ? s.ptyName + (s.ptyDynamic ? ` (${t('ens_dynamic')})` : '') : ''],
    [t('language'), s.language ?? ''],
    [t('announcements'), s.asu.join(', ')],
    [t('ens_long_label'), s.long ?? ''],
  ]
  return (
    <div className="svc-details">
      <div className="facts">
        {facts
          .filter(([, v]) => v)
          .map(([k, v]) => (
            <span key={k}>
              {k} <b>{v}</b>
            </span>
          ))}
        {s.ca && <span className="badge warn">CA</span>}
      </div>
      {s.components.map((c, i) => {
        const sc = sub(c.subch)
        return (
          <div key={i} className="comp">
            <b>{c.tmid === 0 ? t('audio') : t('data')}</b>
            <span className="hint">
              {' '}
              {c.primary ? t('ens_primary') : t('ens_secondary')} · SCIdS {c.scids ?? '?'}
              {c.label && c.label !== s.label ? ` · ${c.label}` : ''}
            </span>
            <div className="mono">
              {c.tmid === 0 ? `ASCTy 0x${hex(c.ctype, 2)}` : c.type}
              {c.packetAddress !== null ? ` · packet ${c.packetAddress}` : ''}
              {sc ? ` · ${t('subchannel')} ${sc.id}: CU ${sc.start}–${sc.start + sc.size - 1}, ${sc.protection}${sc.codeRate ? ` ${sc.codeRate}` : ''}, ${sc.bitrate} kbps` : ''}
              {c.language ? ` · ${c.language}` : ''}
            </div>
            {c.userApps.map((u, j) => (
              <div key={j} className="mono">
                <span className="app-badge">{appName(u.name)}</span> UAType 0x{hex(u.type, 3)}
                {u.xpadAppType !== null ? ` · X-PAD ${u.xpadAppType}` : ''}
                {u.dscty !== null ? ` · DSCTy 0x${hex(u.dscty, 2)}` : ''}
                {u.data ? ` · ${u.data}` : ''}
              </div>
            ))}
          </div>
        )
      })}
      {s.linking.links.length > 0 && (
        <div className="comp">
          <b>{t('service_linking')}</b>
          <div>{s.linking.links.map((l) => `${l.kind} ${l.id}${l.label ? ` (${l.label})` : ''} ${l.hard ? 'hard' : 'soft'}${l.active ? '' : ' inactive'} LSN ${l.lsn}`).join(' · ')}</div>
        </div>
      )}
      {s.linking.frequencies.length > 0 && (
        <div className="comp">
          <b>{t('frequency_info')}</b>
          <div>
            {s.linking.frequencies
              .map((f) => `${f.kind} ${f.ref}: ${f.frequencies.map((x) => (f.kind === 'FM' ? fmMhz(x.khz) : f.kind === 'DAB' ? `${x.channel ?? ''} ${mhz(x.khz)}` : `${x.khz} kHz`)).join(', ')}`)
              .join(' · ')}
          </div>
        </div>
      )}
      {s.linking.ensembles.length > 0 && (
        <div className="comp">
          <b>{t('oe_services')}</b>
          <div>{s.linking.ensembles.map((e) => `${e.eid}${e.frequencies.length ? ' (' + e.frequencies.map((x) => x.channel ?? mhz(x.khz)).join(', ') + ')' : ''}`).join(', ')}</div>
        </div>
      )}
    </div>
  )
}

/** The services in the order of their sub-channels; a click opens the details. */
function ServiceTable({ ens, playing, sel, color, onPick }: { ens: Ensemble; playing?: number; sel: number | null; color: (id: number) => string; onPick: (id: number) => void }) {
  const t = useT()
  const [open, setOpen] = useState<number | null>(null)
  const sub = (id: number | null) => ens.subchannels?.find((x) => x.id === id)
  const services = [...(ens.services ?? [])].sort((a, b) => (sub(a.subchannel)?.start ?? CUS) - (sub(b.subchannel)?.start ?? CUS) || a.label.localeCompare(b.label))
  return (
    <div className="card ens-services">
      <div className="card-title">
        {t('services')} ({services.length})
      </div>
      <div className="table-wrap">
        <table className="table ens-table">
          <thead>
            <tr>
              <th>{t('service')}</th>
              <th>SId</th>
              <th>{t('subchannel')}</th>
              <th>CU</th>
              <th className="num">kbps</th>
              <th>{t('protection')}</th>
              <th>{t('codec')}</th>
              <th>{t('pty')}</th>
              <th>{t('ens_apps')}</th>
            </tr>
          </thead>
          <tbody>
            {services.map((s) => {
              const sc = sub(s.subchannel)
              const apps = [...new Set(s.components.flatMap((c) => c.userApps.map((u) => appName(u.name))))]
              const isOpen = open === s.sid
              return (
                <Fragment key={s.sid}>
                  <tr
                    className={[s.sid === playing ? 'playing' : '', sc && sc.id === sel ? 'sel' : '', isOpen ? 'open' : ''].join(' ')}
                    onClick={() => {
                      setOpen(isOpen ? null : s.sid)
                      if (sc) onPick(sc.id)
                    }}
                  >
                    <td>
                      <div className="svc-cell">
                        <Logo label={s.label} sid={s.sid} small />
                        <span>
                          <b>{s.label}</b>
                          {s.short && s.short !== s.label && <small>{s.short}</small>}
                        </span>
                      </div>
                    </td>
                    <td className="mono">{s.sidHex}</td>
                    <td>
                      {sc ? (
                        <>
                          <i className="swatch" style={{ background: color(sc.id) }} />
                          {sc.id}
                        </>
                      ) : (
                        '–'
                      )}
                    </td>
                    <td className="mono">{sc ? `${sc.start}–${sc.start + sc.size - 1}` : '–'}</td>
                    <td className="num">{s.bitrate ?? sc?.bitrate ?? '–'}</td>
                    <td>{sc?.protection ?? s.protection ?? '–'}</td>
                    <td>{s.isAudio ? (s.codec ?? t('audio')) : t('data')}</td>
                    <td>{s.ptyName ?? '–'}</td>
                    <td>
                      {apps.map((a) => (
                        <span key={a} className="app-badge">
                          {a}
                        </span>
                      ))}
                    </td>
                  </tr>
                  {isOpen && (
                    <tr className="details">
                      <td colSpan={9}>
                        <ServiceDetails s={s} ens={ens} sub={sub} />
                      </td>
                    </tr>
                  )}
                </Fragment>
              )
            })}
          </tbody>
        </table>
      </div>
    </div>
  )
}

/** What the multiplex says of other frequencies, ensembles and services (FIG 0/21, 0/6, 0/24, 0/19). */
function NetworkInfo({ ens }: { ens: Ensemble }) {
  const t = useT()
  const freqs = ens.frequencyInfo ?? []
  const links = ens.linkage ?? []
  const oe = ens.oeServices ?? []
  const ann = ens.announcements ?? []
  if (!freqs.length && !links.length && !oe.length && !ann.length) return null
  const cached = (c?: boolean) => c && <CacheTag time={ens.cacheTime} />
  return (
    <div className="ens-net">
      {freqs.length > 0 && (
        <div className="card">
          <div className="card-title">{t('frequency_info')}</div>
          {freqs.map((f, i) => (
            <div key={i} className="net-row">
              <b>
                {f.oe ? 'OE ' : ''}
                {f.type} {f.id}
              </b>
              {f.label ? ` ${f.label}` : ''}:{' '}
              {f.frequencies.map((x) => (f.rm <= 1 ? `${x.channel ?? ''} ${mhz(x.khz)}` : f.rm >= 8 && f.rm <= 9 ? `${fmMhz(x.khz)} MHz` : `${x.khz} kHz`)).join(', ')}
              {f.continuity ? ` · ${t('ens_continuous')}` : ''}
              {f.regionId ? ` · region ${f.regionId}` : ''} {cached(f.cached)}
            </div>
          ))}
        </div>
      )}
      {links.length > 0 && (
        <div className="card">
          <div className="card-title">{t('service_linking')}</div>
          {links.map((l, i) => (
            <div key={i} className="net-row">
              <b>LSN {l.lsn}</b> {l.hard ? 'hard' : 'soft'}
              {l.active ? '' : ' inactive'}
              {l.international ? ' international' : ''}
              {l.oe ? ' OE' : ''}
              {l.keySid ? ` · ${l.keyLabel ?? l.keySid}` : ''}: {l.ids.map((x) => `${x.kind} ${x.ecc ? x.ecc + '/' : ''}${x.id}`).join(', ')} {cached(l.cached)}
            </div>
          ))}
        </div>
      )}
      {oe.length > 0 && (
        <div className="card">
          <div className="card-title">{t('oe_services')}</div>
          {oe.map((o, i) => (
            <div key={i} className="net-row">
              <b>{o.label ?? o.sid}</b> <span className="mono">{o.sid}</span> →{' '}
              {o.ensembles.map((e) => `${e.eid}${e.channels.filter(Boolean).length ? ' ' + e.channels.filter(Boolean).join(',') : ''}`).join(', ')} {cached(o.cached)}
            </div>
          ))}
        </div>
      )}
      {ann.length > 0 && (
        <div className="card">
          <div className="card-title">{t('announcements')}</div>
          {ann.map((a, i) => (
            <div key={i} className="net-row">
              <b>{a.types.join(', ')}</b> · {t('subchannel')} {a.subch} · cluster {a.cluster}
              {a.newFlag ? ` · ${t('ens_new')}` : ''}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

export default function InfoView() {
  const t = useT()
  const ens = useStore((s) => s.ensemble)
  const selected = useStore((s) => s.selected)
  const svc = ens.services?.find((s) => s.sid === selected?.sid)
  const [selSubch, setSelSubch] = useState<number | null>(null)
  const admin = useStore((s) => s.admin)
  const raw = useStore((s) => s.rawRecord)
  const dx = useStore((s) => s.dxrec)
  // (raw IQ only from a tuner or an IQ recording, not from ETI/EDI or the DTR-100)
  const iq = useStore((s) => (s.signal?.input ?? 'rf') === 'rf')
  const token = useStore((s) => s.chatToken)
  const [rawSecs, setRawSecs] = useState(60)
  // (the recordings made before, to download)
  useEffect(() => {
    if (admin) send({ type: 'rawRecord', action: 'list' })
  }, [admin])
  const subchId = selSubch ?? svc?.subchannel ?? null
  const order = (ens.subchannels ?? []).map((x) => x.id)
  const color = (id: number) => SUBCH_COLORS[Math.max(0, order.indexOf(id)) % SUBCH_COLORS.length]

  return (
    <>
      <ViewHeader />
      <div className="view-body">
        {ens.valid ? (
          <>
            <div className="ens-top">
              <EnsembleHead ens={ens} />
              <NowCard svc={svc} />
            </div>
            <CuMap ens={ens} sel={subchId} color={color} onPick={setSelSubch} />
            <ServiceTable ens={ens} playing={svc?.sid} sel={subchId} color={color} onPick={setSelSubch} />
            <NetworkInfo ens={ens} />
          </>
        ) : (
          <div className="hint">{t('no_signal')}</div>
        )}
        <div style={{ marginTop: 10, display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
          <button className="btn" disabled={!ens.valid} onClick={() => downloadText(`${ens.channel}_${ens.eidHex}.csv`, ensembleCsv(ens))}>
            {t('export_csv')}
          </button>
          <span style={{ flex: 1 }} />
          {admin && (
            <>
              <input className="input" type="number" min={1} max={3600} value={rawSecs} style={{ width: 80 }} onChange={(e) => setRawSecs(parseInt(e.target.value) || 60)} />
              <span className="hint">s</span>
              {raw?.active ? (
                <button className="btn rec on" onClick={() => send({ type: 'rawRecord', action: 'stop' })}>
                  ● {raw.position?.toFixed(0)}/{raw.length?.toFixed(0)} s
                </button>
              ) : (
                <button className="btn" disabled={!iq} title={iq ? undefined : t('raw_unavailable')} onClick={() => send({ type: 'rawRecord', action: 'start', seconds: rawSecs })}>
                  {t('record_raw')}
                </button>
              )}
              {dx?.active ? (
                <button className="btn rec on" onClick={() => send({ type: 'dxrec', action: 'stopRecording' })} title={t('eti_saved_in')}>
                  ● ETI {formatDuration(dx.active.seconds)}/{formatDuration(dx.active.limit)}
                </button>
              ) : (
                <button className="btn" disabled={!ens.valid} title={t('eti_saved_in')} onClick={() => send({ type: 'dxrec', action: 'record', seconds: rawSecs })}>
                  {t('record_eti')}
                </button>
              )}
              {raw && !raw.active && raw.file === undefined ? null : raw?.file ? <span className="hint">{raw.file}</span> : null}
            </>
          )}
        </div>
        {admin && !!raw?.files?.length && (
          <div className="raw-files">
            <div className="hint">{t('raw_files')}</div>
            {raw.files.map((f) => (
              <a key={f.name} href={`${location.pathname.replace(/[^/]*$/, '')}raw/${encodeURIComponent(f.name)}?token=${encodeURIComponent(token ?? '')}`} download={f.name}>
                {f.name} <span className="hint">{(f.size / 1048576).toFixed(0)} MB</span>
              </a>
            ))}
          </div>
        )}
      </div>
    </>
  )
}
