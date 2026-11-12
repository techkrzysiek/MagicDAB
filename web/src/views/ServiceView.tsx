import { useMemo, useState } from 'react'
import { ArrowLeftRight, ChevronLeft, ChevronRight, Globe, ImageOff, Link2, ListTree, Loader2, Lock, Music2 } from 'lucide-react'
import { useStore, useChannelAllowed } from '../store'
import type { SlideProgress } from '../store'
import { useT } from '../i18n'
import { Logo, Flag, FavStar, CacheTag, SignalBars, useDismiss, scrollIntoList, safeUrl } from '../components/common'
import { TuneLockButton } from '../components/TuneDialog'
import { selectService, tune, stepChannel, tuningLocked } from '../actions'
import { startRecording, stopRecording } from '../audio/recorder'
import { player } from '../audio/player'
import { audioCodec } from '../audio/speaker'
import { useEffect, useRef } from 'react'
import { audioCoding, fullSid, hex, mhz, fmMhz, ageShort } from '../util'
import type { Service, Linking, LinkInfo } from '../types'
import { StationBrowser } from './StationsView'
import { useSlideLogoUrl } from '../components/logo'

const EMPTY: string[] = []

/** True on phone sized screens (same breakpoint as the CSS). */
function useIsMobile() {
  const q = '(max-width: 760px)'
  const [m, setM] = useState(() => window.matchMedia(q).matches)
  useEffect(() => {
    const mq = window.matchMedia(q)
    const f = () => setM(mq.matches)
    mq.addEventListener('change', f)
    return () => mq.removeEventListener('change', f)
  }, [])
  return m
}

function codecLabel(codec?: string | null, sbr?: boolean, ps?: boolean) {
  if (codec === 'DAB') return 'MP2'
  if (codec === 'DAB+' || codec?.startsWith('HE') || codec === 'AAC-LC') {
    if (ps) return 'HE-AAC v2'
    if (sbr) return 'HE-AAC'
    return codec === 'DAB+' ? 'AAC' : codec === 'AAC-LC' ? 'AAC-LC' : codec
  }
  return codec ?? ''
}

function fmtFreq(kind: string, f: { khz: number; channel: string | null }) {
  if (kind === 'FM') return `${fmMhz(f.khz)} MHz`
  if (kind === 'DAB') return f.channel ? `${f.channel} (${mhz(f.khz)})` : `${mhz(f.khz)} MHz`
  return `${f.khz} kHz`
}

function LinkingInfo({ linking, cacheTime }: { linking: Linking; cacheTime?: number }) {
  const t = useT()
  const { links, frequencies, ensembles } = linking
  if (!links.length && !frequencies.length && !ensembles.length) return null
  return (
    <div className="link-box">
      {links.length > 0 && (
        <div className="link-sec">
          <h4 title={t('linking_help')}>{t('service_linking_short')}</h4>
          {links.map((l, i) => (
            <div key={i} className="link-row">
              <span className="link-kind">{l.kind}</span>
              <span>
                {l.kind === 'FM' ? 'PI ' : l.kind === 'DAB' ? 'SId ' : ''}
                <b>{l.id}</b>
                {l.label ? ` ${l.label}` : ''}
                {l.ecc ? <span className="hint"> ECC {l.ecc}</span> : null}
              </span>
              <span className={'chip' + (l.hard ? ' hard' : '')} title={l.hard ? t('hard_help') : t('soft_help')}>
                {l.hard ? t('hard_link') : t('soft_link')}
              </span>
              {!l.active && <span className="hint">{t('inactive')}</span>}
              {l.cached && <CacheTag time={cacheTime} />}
            </div>
          ))}
        </div>
      )}
      {frequencies.length > 0 && (
        <div className="link-sec">
          <h4 title={t('fi_help')}>{t('alt_frequencies')}</h4>
          {frequencies.map((f, i) => (
            <div key={i} className="link-row">
              <span className="link-kind">{f.kind}</span>
              <span className="hint">{f.kind === 'FM' ? `PI ${f.ref}` : f.kind === 'DAB' ? `EId ${f.ref}` : f.ref}</span>
              <span>
                {f.frequencies.map((x) => (
                  <span key={x.khz} className="chip">
                    {fmtFreq(f.kind, x)}
                  </span>
                ))}
                {f.cached && <CacheTag time={cacheTime} />}
              </span>
            </div>
          ))}
        </div>
      )}
      {ensembles.length > 0 && (
        <div className="link-sec">
          <h4 title={t('oe_help')}>{t('also_on')}</h4>
          <div className="link-row">
            {ensembles.some((e) => e.cached) && <CacheTag time={cacheTime} />}
            {ensembles.map((e) => (
              <span key={e.eid} className="chip" title={e.frequencies.map((x) => fmtFreq('DAB', x)).join(', ')}>
                EId {e.eid}
                {e.frequencies.length ? ', ' + e.frequencies.map((x) => x.channel ?? mhz(x.khz)).join(', ') : ''}
              </span>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

/**
 * Level of the programme: loudness as in broadcasting (EBU R128, LUFS:
 * momentary as the bar, short-term as the number) and the sample peak (dBFS,
 * held for 2 s); a plain peak bar where the browser cannot measure loudness.
 */
function LevelMeter() {
  const t = useT()
  const bar = useRef<HTMLDivElement>(null)
  const tick = useRef<HTMLDivElement>(null)
  const silentSince = useRef<number>(Date.now())
  const [silent, setSilent] = useState(false)
  const meter = useStore((s) => s.meter)
  // -40 ... 0 on the bar
  const pos = (db: number) => Math.max(0, Math.min(100, ((db + 40) / 40) * 100))
  useEffect(() => {
    const i = window.setInterval(() => {
      const p = player.peak
      player.peak *= 0.85
      if (!useStore.getState().meter) {
        const db = p > 0 ? 20 * Math.log10(p) : -90
        if (bar.current) bar.current.style.width = `${Math.max(0, Math.min(100, ((db + 60) / 60) * 100))}%`
      }
      if (p > 0.0005) silentSince.current = Date.now()
      setSilent(Date.now() - silentSince.current > 2500)
    }, 120)
    return () => window.clearInterval(i)
  }, [])
  useEffect(() => {
    if (!meter) return
    if (bar.current) bar.current.style.width = `${pos(meter.m)}%`
    if (tick.current) tick.current.style.left = `${pos(meter.max)}%`
  }, [meter])
  const fmt = (v: number) => (Number.isFinite(v) ? v.toFixed(1) : '−∞')
  return (
    <span className="meter-wrap" title={meter ? t('loudness_help') : silent ? t('silence') : ''}>
      <span className="meter">
        <span ref={bar} />
        {meter && <span ref={tick} className={'meter-peak' + (meter.max > -1 ? ' over' : '')} />}
      </span>
      {meter && !silent && (
        <span className="meter-values">
          I <b>{fmt(meter.i)}</b> LUFS, S {fmt(meter.s)}, max <b className={meter.max > -1 ? 'over' : ''}>{fmt(meter.max)}</b> dBFS
        </span>
      )}
      {silent && <span className="silence">{t('silence')}</span>}
    </span>
  )
}

function RecordButtons() {
  const t = useT()
  const rec = useStore((s) => s.recording)
  // (the Opus recording is made of the audio sent to this browser)
  const codec = useStore((s) => audioCodec(s))
  const [, force] = useState(0)
  useEffect(() => {
    if (!rec) return
    const i = window.setInterval(() => force((x) => x + 1), 1000)
    return () => window.clearInterval(i)
  }, [rec])
  if (rec) {
    const secs = Math.floor((Date.now() - rec.started) / 1000)
    return (
      <button className="btn rec on" onClick={stopRecording} title={t('stop_record')}>
        ● {Math.floor(secs / 60)}:{(secs % 60).toString().padStart(2, '0')}, {(rec.bytes / 1048576).toFixed(1)} MB, {rec.format.toUpperCase()}
      </button>
    )
  }
  return (
    <span className="rec-group">
      <button className="btn rec" onClick={() => startRecording('aac')} title={t('record_aac')}>
        ● AAC
      </button>
      {codec === 'opus' && (
        <button className="btn rec" onClick={() => startRecording('opus')} title={t('record_opus')}>
          ● Opus
        </button>
      )}
    </span>
  )
}

/** Signal of another ensemble from the last scan: SNR while fresh, otherwise only the age. */
function MuxSignal({ snr, lastSeen }: { snr: number; lastSeen: number }) {
  const t = useT()
  const lang = useStore((s) => s.lang)
  const age = Date.now() / 1000 - lastSeen
  const fresh = age < 6 * 3600
  const lvl = snr >= 10 ? 3 : snr >= 6 ? 2 : snr > 0 ? 1 : 0
  return (
    <span className={'mux-signal' + (fresh ? '' : ' stale')} title={`${t('last_scan')}: ${new Date(lastSeen * 1000).toLocaleString()}`}>
      {fresh && snr > 0 && (
        <>
          <SignalBars level={lvl} synced />
          <b>{snr.toFixed(1)} dB</b>
        </>
      )}
      <span className="hint">{ageShort(lastSeen, lang)}</span>
    </span>
  )
}

/** Same service (country code + PI/SId) on other ensembles: small icon with a dropdown list. */
function OtherMuxes({ svc }: { svc: Service }) {
  const t = useT()
  const list = useStore((s) => s.serviceList)
  const current = useStore((s) => s.ensemble)
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLSpanElement>(null)
  useDismiss([box], open, () => setOpen(false))
  const allowed = useChannelAllowed()
  const ecc = svc.ecc
  const isCurrent = (e: { eid: number; channel: string }) => current.valid && e.eid === current.eid && e.channel === current.channel
  const items = list
    .filter((e) => allowed(e.channel) || isCurrent(e))
    .filter((e) => e.services.some((x) => x.sid === svc.sid && (x.ecc === null || ecc === null || x.ecc === ecc)))
    .sort((a, b) => Number(isCurrent(b)) - Number(isCurrent(a)) || b.snr - a.snr)
  const others = items.filter((e) => !isCurrent(e))
  if (!others.length) return null
  return (
    <span className="dropdown" ref={box}>
      <button className="icon-btn other-mux-btn" title={t('other_mux')} onClick={() => setOpen(!open)}>
        <ArrowLeftRight />
        <span className="count">{others.length}</span>
      </button>
      {open && (
        <div className="dropdown-menu mux-menu">
          <div className="dropdown-title">
            {t('other_mux')}, {svc.label} ({(ecc ? hex(ecc, 2) : '') + svc.sidHex})
          </div>
          {items.map((e) => {
            const cur = isCurrent(e)
            return (
              <button
                key={e.channel + e.eid}
                className={cur ? 'current' : ''}
                disabled={cur}
                onClick={() => {
                  setOpen(false)
                  selectService(svc.sid, 0, e.channel)
                }}
              >
                <span className="mux-ch">{e.channel}</span>
                <span className="mux-main">
                  <span className="mux-label">{e.label}</span>
                  <span className="hint">
                    {mhz(e.frequency)} MHz, EId {hex(e.ecc, 2)}
                    {hex(e.eid)}
                  </span>
                </span>
                <span className="mux-side">
                  {cur ? (
                    <span className="now-tag">● {t('current_mux')}</span>
                  ) : (
                    <MuxSignal snr={e.snr} lastSeen={e.lastSeen} />
                  )}
                </span>
              </button>
            )
          })}
        </div>
      )}
    </span>
  )
}

function ServicePanel({ svc }: { svc?: Service }) {
  const t = useT()
  const mobile = useIsMobile()
  const cacheTime = useStore((s) => s.ensemble.cacheTime)
  const data = useStore((s) => (svc ? s.services[svc.sid] : undefined))
  const audioEnabled = useStore((s) => s.audioEnabled)
  const audioPlaying = useStore((s) => s.audioPlaying)
  const buffer = useStore((s) => s.audioBuffer)
  const underruns = useStore((s) => s.audioUnderruns)
  const lost = useStore((s) => s.audioLost)
  const codec = useStore((s) => audioCodec(s))
  const speakerError = useStore((s) => s.speakerError)
  const useRadioDns = useStore((s) => s.useRadioDns)
  const [preferVis, setPreferVis] = useState(false)
  const linkInfo = useStore((s) => s.linkInfo)
  const slideLogo = useSlideLogoUrl(svc?.sid)
  // the sub-channel comes (about 4 s of it), not one audio unit decoded
  const st = data?.stats
  const noAudio = !!st && st.frames > 160 && st.auOk + st.auErrors === 0
  // audio units come, but not one survived the error correction (a weak
  // signal, also in a DX recording of one): the silence explained
  const tooWeak = !!st && st.frames > 160 && st.auOk === 0 && st.auErrors > 0
  const linkFm = !!svc && linkInfo?.sid === svc.sid && linkInfo.source === 'fm'
  if (!svc) {
    return (
      <div className="service-panel">
        <div className="sp-head">
          <div className="name" style={{ color: 'var(--text-secondary)' }}>{t('no_service')}</div>
        </div>
        <div className="slide-box">
          <div className="placeholder">
            <ImageOff />
          </div>
        </div>
        <div className="hint" style={{ textAlign: 'center' }}>{t('click_to_play')}</div>
      </div>
    )
  }
  const fmt = data?.format
  const dls = data?.dls
  // RadioVIS (pictures over the internet) when the listener allows RadioDNS data;
  // the broadcast slideshow comes first unless chosen otherwise
  const vis = useRadioDns ? data?.visSlide : undefined
  const showVis = !!vis && (!data?.slide || preferVis)
  const slide = showVis ? vis : data?.slide
  const slideLink = safeUrl(slide?.clickThroughUrl)
  const tags = dls?.tags ?? []
  const tag = (name: string) => tags.find((x) => x.name === name)?.text
  const fields: [string, string | undefined][] = [
    [t('title'), tag('ITEM.TITLE')],
    [t('artist'), tag('ITEM.ARTIST')],
    [t('album'), tag('ITEM.ALBUM')],
    [t('now'), tag('PROGRAMME.NOW')],
    [t('next'), tag('PROGRAMME.NEXT')],
    [t('host'), tag('PROGRAMME.HOST')],
  ]
  const stereo = fmt ? (fmt.stereo ? 'STEREO' : 'MONO') : null
  const coding = fmt?.sampleRate ? audioCoding(fmt) : undefined
  const hasLinking = svc.linking.links.length > 0 || svc.linking.frequencies.length > 0 || svc.linking.ensembles.length > 0
  const dlPlus = fields.some(([, v]) => v) && (
    <div className="dlplus">
      {fields.filter(([, v]) => v).map(([k, v]) => (
        <div key={k} style={{ display: 'contents' }}>
          <span className="k">{k}:</span>
          <span>{v}</span>
        </div>
      ))}
    </div>
  )
  const ptyShown = !!(svc.ptyName && svc.pty)
  return (
    <div className="service-panel">
      <div className="sp-head">
        <Logo label={svc.label} sid={svc.sid} onClick={() => useStore.getState().set({ logoDialog: svc.sid })} />
        <div className="name" title={svc.long ?? svc.label}>{svc.long || svc.label}</div>
        <OtherMuxes svc={svc} />
        <FavStar sid={svc.sid} />
      </div>
      <div className="pty-line">
        <Flag code={svc.country} />
        {/* (one item: the flex gap would stand before the comma) */}
        {(ptyShown || svc.language) && (
          <span>
            {ptyShown ? svc.ptyName : null}
            {svc.language && <span className="hint">{ptyShown ? ', ' : ''}{svc.language}</span>}
          </span>
        )}
        {svc.cachedFields?.some((f) => f === 'pty' || f === 'language') && <CacheTag time={cacheTime} />}
      </div>
      <div className="badges">
        {svc.bitrate ? <span className="badge">{svc.bitrate} kbps</span> : null}
        {stereo && <span className="badge">{stereo}</span>}
        <span className="badge" title={coding}>{codecLabel(fmt?.codec ?? svc.codec, fmt?.sbr, fmt?.ps)}</span>
        {fmt?.sampleRate ? <span className="badge" title={`${t('sample_rate')}: ${coding}`}>{fmt.sampleRate / 1000} kHz</span> : null}
        {svc.protection && <span className="badge">{svc.protection}</span>}
        {svc.ca && <span className="badge warn">CA</span>}
        {linkFm && (
          <span className="badge warn" title={t('link_fm_heard')}>
            FM {((linkInfo?.khz ?? 0) / 1000).toFixed(1)}
          </span>
        )}
      </div>
      {svc && linkInfo?.sid === svc.sid && <LinkRow info={linkInfo} />}
      {noAudio && <div className="banner">{t('svc_no_audio')}</div>}
      {tooWeak && <div className="banner">{t('svc_too_weak')}</div>}
      <div className="slide-box">
        {slide ? (
          slideLink ? (
            <a href={slideLink} target="_blank" rel="noreferrer">
              <img src={slide.url} alt={slide.contentName} />
            </a>
          ) : (
            <img src={slide.url} alt={slide.contentName} title={slide.contentName} />
          )
        ) : slideLogo ? (
          // no slideshow (yet): the station's logo, like a car radio
          <>
            <img className="slide-logo" src={slideLogo} alt={svc.label} />
            <span className="vis-badge slide-logo" title={t('slide_logo_hint')}>
              {t('slide_logo')}
            </span>
          </>
        ) : (
          <div className="placeholder">
            <ImageOff />
            <span>{svc.short || svc.label}</span>
          </div>
        )}
        {showVis && (
          <span className="vis-badge" title={t('vis_badge')}>
            RadioVIS
          </span>
        )}
        {vis && data?.slide && (
          <div className="slide-source" title={t('slide_source')}>
            <button className={showVis ? '' : 'on'} onClick={() => setPreferVis(false)}>
              DAB
            </button>
            <button className={showVis ? 'on' : ''} onClick={() => setPreferVis(true)}>
              VIS
            </button>
          </div>
        )}
        <SlideLoading p={data?.slideProgress} />
      </div>
      {dls?.text && (
        <div className="dls">
          <Music2 />
          <span>{dls.text}</span>
        </div>
      )}
      {/* the RadioVIS text under the DLS (unless it says the same) */}
      {useRadioDns && data?.visText && data.visText.trim() !== dls?.text?.trim() && (
        <div className="dls vis" title={t('vis_text')}>
          <Globe />
          <span>{data.visText}</span>
        </div>
      )}
      {!dls?.text && !(useRadioDns && data?.visText) && <div className="dls empty" />}
      {mobile ? (
        (fields.some(([, v]) => v) || hasLinking) && (
          <details className="sp-details">
            <summary>{t('details')}</summary>
            {dlPlus}
            <LinkingInfo linking={svc.linking} cacheTime={cacheTime} />
          </details>
        )
      ) : (
        <>
          {dlPlus}
          <LinkingInfo linking={svc.linking} cacheTime={cacheTime} />
        </>
      )}
      <div className="play-row">
        {audioEnabled ? (
          audioPlaying ? (
            <span className="play-info">
              {/* (one line that does not break; on a phone the meter goes below it) */}
              <span className="play-stats">
                ▶ {codec === 'native' ? t('audio_native_short') : codec.toUpperCase()}, {t('buffer')} {buffer.toFixed(2)} s
                {underruns > 0 && <span className="hint">, {t('underruns')}: {underruns}</span>}
                {lost > 0 && (
                  <span className="hint" title={t('audio_lost_help')}>
                    , {t('audio_lost')}: {lost}
                  </span>
                )}
              </span>
              <LevelMeter />
            </span>
          ) : speakerError && codec === 'native' ? (
            <span className="hint" title={speakerError}>
              {t('audio_native_error')}
            </span>
          ) : (
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              <Loader2 size={14} className="spin" /> {t('buffer')}…
            </span>
          )
        ) : (
          <span>{t('click_to_play')}</span>
        )}
        <span style={{ flex: 1 }} />
        {audioEnabled && <RecordButtons />}
      </div>
    </div>
  )
}

/**
 * The channel: chosen from the list or stepped, its frequency, the lock of the
 * tuner. Beside the stations on a computer, above the multiplex on a phone.
 */
export function ChannelRow({ className }: { className?: string }) {
  const t = useT()
  const state = useStore((s) => s.state)
  const hello = useStore((s) => s.hello)
  const fixed = state ? state.fixedChannel : hello?.fixedChannel
  const allowed = useChannelAllowed()
  // the tuner is locked, own multiplexes are allowed: only the mapped channels
  const mappedOnly = useStore((s) => tuningLocked(s) && !!s.hello?.listenerMultiplex)
  const mapped = state?.mappedChannels ?? EMPTY
  return (
    <div className={'chan-row' + (className ? ' ' + className : '')}>
      <select value={state?.channel ?? ''} onChange={(e) => tune(e.target.value)} disabled={!!fixed} title={fixed ? t('channel_fixed') : undefined}>
        {(hello?.channels ?? [])
          .filter((c) => !state?.noTuner || mapped.includes(c.name))
          .filter((c) => allowed(c.name) || c.name === state?.channel)
          .map((c) => (
            <option key={c.name} value={c.name} disabled={mappedOnly && !mapped.includes(c.name) && c.name !== state?.channel}>
              {c.name}
              {mapped.includes(c.name) ? ', ETI/EDI' : ''}
            </option>
          ))}
      </select>
      {fixed ? (
        <span className="chip fixed-chip" title={t('channel_fixed')}>
          <Lock size={13} /> {t('fixed_source')}
        </span>
      ) : (
        <button className="icon-btn" onClick={() => stepChannel(-1)}>
          <ChevronLeft />
        </button>
      )}
      <span className="freq">{state ? `${mhz(state.frequency)} MHz` : ''}</span>
      {!fixed && (
        <button className="icon-btn" onClick={() => stepChannel(1)}>
          <ChevronRight />
        </button>
      )}
      {!fixed && <TuneLockButton />}
    </div>
  )
}

function ServiceList() {
  const t = useT()
  const ensemble = useStore((s) => s.ensemble)
  const state = useStore((s) => s.state)
  const selected = useStore((s) => s.selected)
  const favorites = useStore((s) => s.favorites)
  const serviceList = useStore((s) => s.serviceList)
  const listMode = useStore((s) => s.listMode)
  const dlsAll = useStore((s) => s.dlsAll)
  const set = useStore((s) => s.set)
  const allowed = useChannelAllowed()
  const services = (ensemble.services ?? []).filter((s) => s.isAudio)
  const listRef = useRef<HTMLUListElement>(null)

  const favRows = useMemo(() => {
    const rows: { sid: number; label: string; channel: string; ecc: number | null }[] = []
    for (const sid of favorites) {
      const cur = services.find((s) => s.sid === sid)
      if (cur) {
        rows.push({ sid, label: cur.label, channel: ensemble.channel ?? '', ecc: cur.ecc })
        continue
      }
      // (on a channel this listener may tune to)
      const e = serviceList.filter((x) => allowed(x.channel) && x.services.some((s) => s.sid === sid)).sort((a, b) => b.lastSeen - a.lastSeen)[0]
      const s = e?.services.find((s) => s.sid === sid)
      if (e && s) rows.push({ sid, label: s.label, channel: e.channel, ecc: s.ecc ?? e.ecc })
    }
    return rows.sort((a, b) => a.label.localeCompare(b.label))
  }, [favorites, services, serviceList, ensemble.channel, allowed])

  // the station playing stays in sight when it changes elsewhere (player buttons, keys)
  const shown = listMode === 'ensemble' ? services.length : favRows.length
  useEffect(() => {
    const li = listRef.current?.querySelector<HTMLElement>('.svc-item.selected')
    if (li) scrollIntoList(li)
  }, [selected?.sid, listMode, shown])

  return (
    <div>
      <div className="sl-head">
        {ensemble.valid ? <Logo label={ensemble.label ?? ''} sid={ensemble.eid ?? 0} small /> : <span className="logo-space" />}
        <div className="ens-name">{ensemble.valid ? ensemble.label : state?.channel ? '…' : ''}</div>
      </div>
      <ChannelRow />
      {!!state?.slot && (
        <div className="own-mux hint">
          {t('own_mux_note')
            .replace('{c}', state.channel ?? '')
            .replace('{m}', state.mainChannel ?? '')}
        </div>
      )}
      <div className="tabs">
        <button className={'tab' + (listMode === 'ensemble' ? ' active' : '')} onClick={() => set({ listMode: 'ensemble' })}>
          {t('all_services')} ({services.length})
        </button>
        <button className={'tab' + (listMode === 'favorites' ? ' active' : '')} onClick={() => set({ listMode: 'favorites' })}>
          {t('favorites')} ({favRows.length})
        </button>
      </div>
      <ul className="svc-list" ref={listRef}>
        {listMode === 'ensemble' &&
          services.map((s) => (
            <li
              key={s.sid}
              className={'svc-item' + (selected?.sid === s.sid ? ' selected' : '')}
              onClick={() => selectService(s.sid, 0)}
            >
              <Logo label={s.label} sid={s.sid} small />
              <div className="txt">
                <div className="lbl">{s.label || s.sidHex}</div>
                <div className="sid" title={dlsAll[s.sid] ?? ''}>
                  {fullSid(s)}
                  {dlsAll[s.sid] ? <span className="dls-line">{dlsAll[s.sid]}</span> : null}
                </div>
              </div>
              <FavStar sid={s.sid} />
            </li>
          ))}
        {listMode === 'favorites' &&
          favRows.map((r) => (
            <li
              key={r.sid}
              className={'svc-item' + (selected?.sid === r.sid ? ' selected' : '')}
              onClick={() => selectService(r.sid, 0, r.channel)}
            >
              <Logo label={r.label} sid={r.sid} ecc={r.ecc} small />
              <div className="txt">
                <div className="lbl">{r.label}</div>
                <div className="sid">
                  SID: {r.ecc ? hex(r.ecc, 2) : ''}
                  {hex(r.sid)}, {r.channel}
                </div>
              </div>
              <FavStar sid={r.sid} />
            </li>
          ))}
      </ul>
    </div>
  )
}

export default function ServiceView() {
  const ensemble = useStore((s) => s.ensemble)
  const selected = useStore((s) => s.selected)
  const [showTree, setShowTree] = useState(false)
  const svc = ensemble.services?.find((s) => s.sid === selected?.sid)
  return (
    <div className={'service-view' + (showTree ? ' show-tree' : '')}>
      <div className="pane">
        <ServicePanel svc={svc} />
      </div>
      <div className="pane list-pane" style={{ position: 'relative' }}>
        <button className="icon-btn tree-toggle" style={{ position: 'absolute', right: 8, top: 12 }} onClick={() => setShowTree(true)}>
          <ListTree />
        </button>
        <ServiceList />
      </div>
      <div className="pane tree-pane" style={{ position: 'relative' }}>
        {showTree && (
          <button className="icon-btn" style={{ position: 'absolute', right: 8, top: 8 }} onClick={() => setShowTree(false)}>
            <ChevronLeft />
          </button>
        )}
        <StationBrowser panel />
      </div>
    </div>
  )
}

/** Thin bar at the bottom of the slideshow while a new slide is received. */
function SlideLoading({ p }: { p?: SlideProgress }) {
  const t = useT()
  const [, tick] = useState(0)
  // segments of slow X-PAD may take long: the bar goes away only when stale
  useEffect(() => {
    if (!p || p.done) return
    const id = setInterval(() => tick((n) => n + 1), 5000)
    return () => clearInterval(id)
  }, [p])
  const live = !!p && !p.done && Date.now() - p.at < 90_000
  const pct = p ? Math.min(100, (p.bytes / p.size) * 100) : 0
  const kb = (n: number) => (n / 1024).toFixed(n < 10240 ? 1 : 0)
  return (
    <div
      className={`slide-loading${live ? ' on' : ''}`}
      title={live ? `${t('slide_receiving')} ${p.name}: ${kb(p.bytes)} / ${kb(p.size)} kB` : undefined}
    >
      <div style={{ width: `${pct}%` }} />
    </div>
  )
}

/** State of the DAB/FM linking of the service; opens the linking view. */
function LinkRow({ info }: { info: LinkInfo }) {
  const t = useT()
  if (!info.targets?.length) return null
  const pis = 'PI ' + info.targets.join('/')
  const mhz = (k?: number | null) => (k ? (k / 1000).toFixed(1) + ' MHz' : '')
  let text: string
  if (!info.enabled) {
    text = t('li_off')
  } else {
    switch (info.phase) {
      case 'locked': {
        const ant = (info.antennas ?? 1) > 1 && info.antenna != null ? `, ${t('li_antenna')} ${info.antenna + 1}` : ''
        const al = info.aligned ? `${t('li_aligned')}${info.delay != null ? `, FM +${info.delay.toFixed(1)} s` : ''}` : t('li_aligning')
        text = `FM ${mhz(info.khz)}${ant}, ${al}, ${t('li_heard')} ${(info.source ?? 'dab').toUpperCase()}`
        break
      }
      case 'scanning':
        text = `${pis}: ${t('li_scanning')}…`
        break
      case 'probing':
        text = `${pis}: ${t('li_probing')} (${info.progress?.left ?? 0})`
        break
      case 'evaluating':
        text = `${pis}: ${t('li_evaluating')} ${info.progress?.done ?? 0}/${info.progress?.total ?? 0}`
        break
      case 'failed':
        text = `${pis}: ${t('li_failed')}`
        break
      default:
        text = info.reason ? `${pis}: ${info.reason}` : `${pis}: ${t('li_searching')}…`
    }
    if (info.mode && info.mode !== 'auto') text += `, ${t('li_mode')} ${info.mode.toUpperCase()}`
  }
  return (
    <button
      className={'link-state' + (info.source === 'fm' ? ' fm' : '') + (info.phase === 'locked' && info.aligned ? ' ok' : '')}
      onClick={() => useStore.getState().set({ view: 'linking' })}
      title={t('link_title')}
    >
      <Link2 size={15} />
      <span>{text}</span>
    </button>
  )
}
