import { useEffect, useRef, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { Logo } from './common'
import { audioCoding } from '../util'
import type { AudioStats, Service } from '../types'

/** Bitrates and error rates between two statistics messages of a station. */
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

export function Metric({ label, value, tone }: { label: string; value: React.ReactNode; tone?: string }) {
  return (
    <div className="metric" style={tone ? { borderColor: tone } : undefined}>
      <span>{label}</span>
      <b>{value}</b>
    </div>
  )
}

/** The station being listened to: what its bitrate carries (audio, PAD, the
 *  overhead of the error correction) and how well it arrives. In the Info view
 *  and in the tooltip of the bitrate in the station panel. */
export function NowDetails({ svc }: { svc?: Service }) {
  const t = useT()
  const data = useStore((s) => (svc ? s.services[svc.sid] : undefined))
  const rates = useRates(data?.stats)
  const st = data?.stats
  const fmt = data?.format
  if (!svc) {
    return (
      <>
        <div className="card-title">{t('ens_now')}</div>
        <div className="hint">{t('click_to_play')}</div>
      </>
    )
  }
  const total = svc.bitrate ?? 0
  const audio = Math.max(0, rates.audio - rates.pad)
  const pad = rates.pad
  const overhead = rates.audio ? Math.max(0, total - rates.audio) : 0
  const width = (kbps: number) => `${total ? Math.min(100, (kbps / total) * 100) : 0}%`
  return (
    <>
      <div className="card-title">{t('ens_now')}</div>
      <div className="now-svc">
        <Logo label={svc.label} sid={svc.sid} plain={!!svc.scids} small />
        <div>
          <b>{svc.label}</b>
          <div className="hint">
            {svc.codec ?? t('data')}{fmt?.sampleRate ? `, ${fmt.sampleRate / 1000} kHz` : ''}, {total} kbps, {svc.protection ?? ''}
          </div>
          {/* how the audio is coded: what the codec badges say, in full */}
          {fmt?.sampleRate ? (
            <div className="hint">
              {audioCoding(fmt)}, {fmt.stereo ? 'stereo' : 'mono'}
            </div>
          ) : null}
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
        <Metric label={t('ens_audio_errors')} value={`${st?.auErrors ?? 0}, ${rates.auErrRate.toExponential(1)}`} tone={st?.auErrors ? 'var(--sig-low)' : undefined} />
      </div>
    </>
  )
}
