import { useEffect, useRef, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { send } from '../ws'
import { selectService, startAudio } from '../actions'
import { formatDuration, whenShort } from '../util'
import type { DxRecording } from '../types'

/** Address of the server (the page may live under a path). */
const base = location.pathname.replace(/[^/]*$/, '')

function mb(bytes: number) {
  return (bytes / 1048576).toFixed(1)
}

/**
 * DX recordings (admins): multiplexes heard for the first time, recorded as
 * ETI files; each can be played back on a receiver of this admin's own (one
 * click on a station of it plays that one), downloaded or deleted, and an
 * .eti or .edi file of one's own can be loaded.
 */
export default function DxRecordings() {
  const t = useT()
  const dx = useStore((s) => s.dxrec)
  const playback = useStore((s) => s.state?.playback ?? null)
  const token = useStore((s) => s.chatToken)
  const ensemble = useStore((s) => s.ensemble)
  const notify = useStore((s) => s.notify)
  const audioOn = useStore((s) => s.audioEnabled)
  const selected = useStore((s) => s.selected?.sid)
  const located = useStore((s) => !!s.hello?.location)
  const [uploading, setUploading] = useState(false)
  const fileInput = useRef<HTMLInputElement>(null)

  useEffect(() => {
    send({ type: 'dxrec', action: 'list' })
  }, [])

  const download = (r: DxRecording) => {
    const a = document.createElement('a')
    a.href = `${base}dxrec/${encodeURIComponent(r.name)}?token=${encodeURIComponent(token ?? '')}`
    a.download = r.name
    document.body.appendChild(a)
    a.click()
    a.remove()
  }

  const upload = async (file: File) => {
    setUploading(true)
    try {
      const r = await fetch(`${base}dxrec/upload?token=${encodeURIComponent(token ?? '')}&name=${encodeURIComponent(file.name)}`, { method: 'POST', body: file })
      if (!r.ok) notify('error', 'dxrec_upload_failed')
    } catch {
      notify('error', 'dxrec_upload_failed')
    } finally {
      setUploading(false)
      send({ type: 'dxrec', action: 'list' })
    }
  }

  // (the sound starts within the click: browsers allow it only there)
  const play = (r: DxRecording, sid?: number) => {
    if (!audioOn) startAudio()
    if (sid != null && playback === r.name) selectService(sid)
    else send({ type: 'dxrec', action: 'play', name: r.name, sid })
  }

  const list = dx?.recordings ?? []
  const active = dx?.active ?? null
  const reason = (r: DxRecording) =>
    r.reason === 'far' ? t('dxrec_reason_far').replace('{km}', r.distance != null ? String(Math.round(r.distance)) : '?') : r.reason ? t('dxrec_reason_' + r.reason) : ''
  const ended = (e: string) => (e && e !== 'complete' ? t('dxrec_ended_' + e) : '')

  return (
    <>
      <div className="section-title">{t('dxrec_tab')}</div>
      <div style={{ display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap', marginBottom: 12 }}>
        <label style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
          <input type="checkbox" checked={!!dx?.enabled} onChange={(e) => send({ type: 'dxrec', action: 'settings', enabled: e.target.checked })} />
          {t('dxrec_auto')}
        </label>
        <span style={{ display: 'flex', gap: 6, alignItems: 'center' }} title={t('dxrec_far_help')}>
          <label style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="checkbox" checked={dx?.far ?? true} onChange={(e) => send({ type: 'dxrec', action: 'settings', far: e.target.checked })} />
            {t('dxrec_far')}
          </label>
          <input
            type="number"
            className="input"
            min={10}
            max={3000}
            step={10}
            style={{ width: 72 }}
            key={dx?.farKm}
            defaultValue={dx?.farKm ?? 200}
            onBlur={(e) => {
              const v = parseInt(e.target.value)
              if (v && v !== dx?.farKm) send({ type: 'dxrec', action: 'settings', farKm: v })
            }}
          />
          km
          {dx?.far && !located && <span className="hint">({t('dxrec_far_no_location')})</span>}
        </span>
        <label style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
          {t('dxrec_minutes')}
          <input
            type="number"
            className="input"
            min={1}
            max={30}
            style={{ width: 64 }}
            key={dx?.minutes}
            defaultValue={dx?.minutes ?? 1}
            onBlur={(e) => {
              const v = parseInt(e.target.value)
              if (v && v !== dx?.minutes) send({ type: 'dxrec', action: 'settings', minutes: v })
            }}
          />
          min
        </label>
        <label style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
          {t('dxrec_cap')}
          <input
            type="number"
            className="input"
            min={100}
            step={100}
            style={{ width: 84 }}
            key={dx?.maxMb}
            defaultValue={dx?.maxMb ?? 2048}
            onBlur={(e) => {
              const v = parseInt(e.target.value)
              if (v && v !== dx?.maxMb) send({ type: 'dxrec', action: 'settings', maxMb: v })
            }}
          />
          MB
        </label>
        <span style={{ flex: 1 }} />
        {active ? (
          <button className="btn rec on" onClick={() => send({ type: 'dxrec', action: 'stopRecording' })} title={t('stop_record')}>
            ● {active.channel} {active.label}, {formatDuration(active.seconds)} / {formatDuration(active.limit)}, {mb(active.bytes)} MB
          </button>
        ) : (
          <button className="btn" disabled={!ensemble.valid} onClick={() => send({ type: 'dxrec', action: 'record' })}>
            {t('dxrec_record_now')}
          </button>
        )}
      </div>
      {playback && (
        <div style={{ display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap', marginBottom: 12 }}>
          <span>{t('dxrec_playing').replace('{n}', playback)}</span>
          <button className="btn danger" onClick={() => send({ type: 'dxrec', action: 'stopPlayback' })}>
            {t('dxrec_stop_play')}
          </button>
        </div>
      )}
      {list.length === 0 ? (
        <p className="hint">{t('dxrec_none')}</p>
      ) : (
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th>{t('dxrec_when')}</th>
                <th>{t('channel')}</th>
                <th>{t('ensemble')}</th>
                <th>{t('services')}</th>
                <th>SNR</th>
                <th>{t('dxrec_length')}</th>
                <th>{t('dxrec_size')}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {list.map((r) => (
                <tr key={r.name} className={playback === r.name ? 'sel' : ''}>
                  <td title={r.name}>{whenShort(r.time)}</td>
                  <td>{r.channel}</td>
                  <td>
                    <b>{r.label}</b> <span className="hint">{r.ecc && r.eid ? `${r.ecc}${r.eid}` : ''}</span>
                    <div className="hint">
                      {reason(r)}
                      {r.tii.length ? `, TII ${r.tii.join(', ')}` : ''}
                      {r.side ? `, ${t('dxrec_side')}` : ''}
                      {ended(r.ended) ? `, ${ended(r.ended)}` : ''}
                    </div>
                  </td>
                  <td style={{ whiteSpace: 'normal' }}>
                    <div style={{ minWidth: 160, maxWidth: 440 }}>
                      {r.services
                        .filter((s) => s.codec !== 'data')
                        .sort((a, b) => a.label.localeCompare(b.label))
                        .map((s) => {
                          const sid = parseInt(s.sid, 16)
                          const on = playback === r.name && selected === sid
                          return (
                            <button
                              key={s.sid}
                              className={'chip chip-btn' + (on ? ' hard' : '')}
                              title={`${s.sid}, ${s.codec} ${s.bitrate} kbps`}
                              onClick={() => play(r, sid)}
                            >
                              {s.label || s.sid}
                            </button>
                          )
                        })}
                    </div>
                  </td>
                  <td>{r.snr ? `${r.snr.toFixed(1)} dB` : ''}</td>
                  <td>{formatDuration(r.duration)}</td>
                  <td>{mb(r.size)} MB</td>
                  <td style={{ whiteSpace: 'nowrap' }}>
                    <button className="btn primary" disabled={playback === r.name} onClick={() => play(r)}>
                      {t('dxrec_play')}
                    </button>{' '}
                    <button className="btn" onClick={() => download(r)}>
                      {t('dxrec_download')}
                    </button>{' '}
                    <button className="btn" onClick={() => confirm(`${t('remove')} ${r.name}?`) && send({ type: 'dxrec', action: 'delete', name: r.name })}>
                      {t('remove')}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <div style={{ display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap', marginTop: 12 }}>
        <input
          ref={fileInput}
          type="file"
          accept=".eti,.edi"
          style={{ display: 'none' }}
          onChange={(e) => {
            const f = e.target.files?.[0]
            if (f) upload(f)
            e.target.value = ''
          }}
        />
        <button className="btn" disabled={uploading} onClick={() => fileInput.current?.click()}>
          {uploading ? t('dxrec_uploading') : t('dxrec_upload')}
        </button>
        <span className="hint">{t('dxrec_upload_hint').replace('{mb}', String(dx?.uploadMaxMb ?? 256))}</span>
        <span style={{ flex: 1 }} />
        {dx?.usedMb != null && <span className="hint">{t('dxrec_space').replace('{used}', String(dx.usedMb)).replace('{max}', String(dx.maxMb))}</span>}
      </div>
    </>
  )
}
