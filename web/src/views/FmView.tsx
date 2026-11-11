import { useMemo, useState } from 'react'
import { ChevronLeft, ChevronRight, Headphones, ScanSearch } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { FM_SID, fmAntenna, fmScan, fmTune, selectService } from '../actions'

const mhz = (khz: number) => (khz / 1000).toFixed(1)

/** Local maxima of the scan: frequencies with a station. */
function peaks(points: [number, number][]): [number, number][] {
  const out: [number, number][] = []
  for (let i = 0; i < points.length; i++) {
    const [f, l] = points[i]
    const prev = points[i - 1]?.[1] ?? -Infinity
    const next = points[i + 1]?.[1] ?? -Infinity
    if (l >= 30 && l >= prev && l > next) out.push([f, l])
  }
  return out.sort((a, b) => b[1] - a[1]).slice(0, 40).sort((a, b) => a[0] - b[0])
}

export default function FmView() {
  const t = useT()
  const fm = useStore((s) => s.fm)
  const scan = useStore((s) => s.fmScan)
  const playing = useStore((s) => s.selected?.sid === FM_SID && s.audioEnabled)
  const [input, setInput] = useState('')
  const found = useMemo(() => peaks(scan), [scan])
  if (!fm?.connected) {
    return (
      <div className="view fm-view">
        <div className="fm-empty">{t('fm_none')}</div>
      </div>
    )
  }
  const rds = fm.rds
  const level = Math.max(0, Math.min(100, fm.signal.level))
  const maxScan = Math.max(1, ...scan.map((p) => p[1]))
  const pi = rds.pi != null ? rds.pi.toString(16).toUpperCase().padStart(4, '0') : null
  return (
    <div className="view fm-view">
      <div className="fm-panel">
        <div className="fm-tune">
          <button className="icon-btn" onClick={() => fmTune(fm.khz - 100)} aria-label="-0.1">
            <ChevronLeft />
          </button>
          <div className="fm-freq">{mhz(fm.khz)}<span> MHz</span></div>
          <button className="icon-btn" onClick={() => fmTune(fm.khz + 100)} aria-label="+0.1">
            <ChevronRight />
          </button>
          <form
            onSubmit={(e) => {
              e.preventDefault()
              const v = parseFloat(input.replace(',', '.'))
              if (v >= 64 && v <= 108) fmTune(v * 1000)
              setInput('')
            }}
          >
            <input className="fm-input" inputMode="decimal" placeholder="MHz" value={input} onChange={(e) => setInput(e.target.value)} />
          </form>
        </div>
        <div className="fm-ps">{rds.ps || ''}</div>
        <div className="fm-rt">{rds.rt}</div>
        <div className="badges">
          {pi && <span className={'badge' + (fm.piErrors ? ' warn' : '')}>PI {pi}{'?'.repeat(fm.piErrors ?? 0)}</span>}
          {rds.ecc != null && <span className="badge">ECC {rds.ecc.toString(16).toUpperCase()}</span>}
          {rds.country && <span className="badge">{rds.country}</span>}
          {rds.ptyName && <span className="badge">{rds.ptyName}</span>}
          {rds.tp && <span className="badge">TP</span>}
          {rds.ta && <span className="badge warn">TA</span>}
          <span className="badge">{fm.signal.stereo ? 'Stereo' : 'Mono'}</span>
          {rds.ptyn && <span className="badge">{rds.ptyn}</span>}
        </div>
        <div className="fm-signal" title={`CCI ${fm.signal.cci}, ACI ${fm.signal.aci}`}>
          <span>{t('fm_level')}</span>
          <div className="fm-bar">
            <div style={{ width: `${level}%` }} />
          </div>
          <span>{fm.signal.level.toFixed(1)} dBf</span>
        </div>
        {fm.antennas > 1 && (
          <div className="fm-signal">
            <span>{t('fm_antenna')}</span>
            <div className="seg">
              {Array.from({ length: fm.antennas }, (_, i) => (
                <button key={i} className={fm.antenna === i ? 'on' : ''} onClick={() => fmAntenna(i)}>
                  {i + 1}
                </button>
              ))}
            </div>
          </div>
        )}
        <div className="row">
          <button className={'btn primary'} disabled={playing} onClick={() => selectService(FM_SID, 0)}>
            <Headphones size={16} /> {playing ? t('fm_listening') : t('fm_listen')}
          </button>
          <button className="btn" onClick={() => fmScan()}>
            <ScanSearch size={16} /> {t('fm_scan')}
          </button>
        </div>
        {rds.af.length > 0 && (
          <div className="fm-section">
            <div className="hint">{t('fm_af')}</div>
            <div className="fm-chips">
              {rds.af.map((f) => (
                <button key={f} className={'chip' + (f === fm.khz ? ' on' : '')} onClick={() => fmTune(f)}>
                  {mhz(f)}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
      {scan.length > 0 && (
        <div className="fm-panel">
          <div className="fm-scan">
            {scan.map(([f, l]) => (
              <div
                key={f}
                className={'fm-scan-bar' + (Math.abs(f - fm.khz) < 50 ? ' on' : '')}
                style={{ height: `${(100 * Math.max(0, l)) / maxScan}%` }}
                title={`${mhz(f)} MHz: ${l.toFixed(0)} dBf`}
                onClick={() => fmTune(f)}
              />
            ))}
          </div>
          <div className="hint">{t('fm_peaks')}</div>
          <div className="fm-chips">
            {found.map(([f, l]) => (
              <button key={f} className={'chip' + (f === fm.khz ? ' on' : '')} onClick={() => fmTune(f)} title={`${l.toFixed(0)} dBf`}>
                {mhz(f)}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}
