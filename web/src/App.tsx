import { useEffect, useRef, useState } from 'react'
import {
  Headphones, Layers, CalendarClock, GalleryVerticalEnd, MapPinned, ScanSearch, AudioWaveform, SlidersHorizontal,
  Ellipsis, Volume2, VolumeX, ScrollText, Info as InfoIcon, LogIn, LogOut, Play, Square, Users, Radio, FileCode2, X, RadioReceiver, Link2,
  MessagesSquare, LockKeyhole, Activity, Keyboard, Gauge,
} from 'lucide-react'
import { useStore, type View } from './store'
import { desktop, desktopToken } from './desktop'
import { useT, weekday } from './i18n'
import { setVolume, toggleMute, toggleAudio, logout, stopScan, tuningLocked } from './actions'
import { setSubscriptions, send, reconnectAfterKick } from './ws'
import { SignalBars, Logo, useDismiss, Markdown, MagicDabLogo } from './components/common'
import { signalLevel, findService } from './util'
import ServiceView from './views/ServiceView'
import InfoView from './views/InfoView'
import TiiView from './views/TiiView'
import SignalView from './views/SignalView'
import ScannerView from './views/ScannerView'
import SettingsView from './views/SettingsView'
import { LogView, AboutView } from './views/MiscViews'
import StatsView from './views/StatsView'
import { LogoDialog } from './components/logo'
import { LoginDialog } from './components/LoginDialog'
import GuideView from './views/GuideView'
import SlidesView from './views/SlidesView'
import { DesktopClose } from './components/DesktopClose'
import StationsView from './views/StationsView'
import SpiView from './views/SpiView'
import LinkingView from './views/LinkingView'
import { ChatPopup } from './components/ChatPopup'
import { TuneDialog } from './components/TuneDialog'
import { ListenersDialog } from './components/Listeners'
import { Shortcuts } from './components/Shortcuts'
import { SetupWizard } from './components/SetupWizard'
import { PrivateGate } from './components/PrivateGate'
import { StillHere } from './components/StillHere'
import type { Signal } from './types'

function useThemeEffect() {
  const theme = useStore((s) => s.theme)
  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const apply = () => {
      const dark = theme === 'dark' || (theme === 'auto' && mq.matches)
      document.documentElement.dataset.theme = dark ? 'dark' : 'light'
    }
    apply()
    mq.addEventListener('change', apply)
    return () => mq.removeEventListener('change', apply)
  }, [theme])
}

function NavRail() {
  const t = useT()
  const view = useStore((s) => s.view)
  const set = useStore((s) => s.set)
  const admin = useStore((s) => s.admin)
  const hasEpg = useStore((s) => Object.keys(s.epgIndex).length > 0)
  // the station playing has shown slides
  const hasSlides = useStore((s) => !!(s.selected && s.services[s.selected.sid]?.slide))
  const fmPresent = useStore((s) => !!s.fm?.connected)
  // (linking is the admin's: without logging in its view is not there)
  const linkingOn = useStore((s) => s.admin && s.linkInfo?.enabled !== false)
  const chatOn = useStore((s) => !!s.hello?.chat)
  const unread = useStore((s) => s.chat.unread)
  const chatOpen = useStore((s) => s.chatOpen)
  // the tuner is locked, a tuning password exists
  const canUnlock = useStore((s) => tuningLocked(s) && !!s.hello?.tunePassword)
  const toggleChat = () => {
    set({ chatOpen: !useStore.getState().chatOpen })
    setMenu(false)
  }
  const [menu, setMenu] = useState(false)
  const moreBtn = useRef<HTMLButtonElement>(null)
  const sheet = useRef<HTMLDivElement>(null)
  useDismiss([moreBtn, sheet], menu, () => setMenu(false))
  const go = (v: View) => {
    set({ view: v })
    setMenu(false)
  }
  // items marked desk are reached through "More" on phones
  const item = (v: View, icon: JSX.Element, label: string, opts: { disabled?: boolean; desk?: boolean; mobile?: boolean } = {}) => (
    <button
      key={v}
      className={'rail-item' + (view === v ? ' active' : '') + (opts.desk ? ' desk-only' : '') + (opts.mobile ? ' mobile-only' : '')}
      disabled={opts.disabled}
      onClick={() => go(v)}
    >
      <span className="pill">{icon}</span>
      <span className="label">{label}</span>
    </button>
  )
  const moreViews: View[] = ['epg', 'spi', 'tii', 'scanner', 'signal', 'linking', 'settings', 'log', 'about', 'stats']
  return (
    <nav className="rail">
      <div className="brand" title="MagicDAB">
        <MagicDabLogo />
        <span>MagicDAB</span>
      </div>
      {item('service', <Headphones />, t('nav_service'))}
      {/* (on a computer the list of all stations is beside the station) */}
      {item('stations', <Radio />, t('nav_stations'), { mobile: true })}
      {fmPresent && linkingOn && item('linking', <Link2 />, t('nav_linking'), { desk: true })}
      {item('info', <Layers />, t('nav_info'))}
      {item('epg', <CalendarClock />, t('nav_epg'), { disabled: !hasEpg, desk: true })}
      {item('spi', <FileCode2 />, t('nav_spi'), { desk: true })}
      {item('catsls', <GalleryVerticalEnd />, t('nav_catsls'), { disabled: !hasSlides, desk: true })}
      {item('tii', <MapPinned />, t('nav_tii'), { desk: true })}
      {item('scanner', <ScanSearch />, t('nav_scanner'), { desk: true })}
      {item('signal', <AudioWaveform />, t('nav_signal'), { desk: true })}
      {chatOn && (
        <button className={'rail-item desk-only' + (chatOpen ? ' active' : '')} onClick={toggleChat}>
          <span className="pill">
            <MessagesSquare />
            {unread > 0 && <span className="rail-badge">{unread > 99 ? '99+' : unread}</span>}
          </span>
          <span className="label">{t('nav_chat')}</span>
        </button>
      )}
      <div className="rail-spacer" />
      {item('settings', <SlidersHorizontal />, t('nav_settings'), { desk: true })}
      <button
        ref={moreBtn}
        className={'rail-item' + (moreViews.includes(view) ? ' active-mobile' : '') + (['log', 'about', 'stats'].includes(view) ? ' active' : '')}
        onClick={() => setMenu(!menu)}
      >
        <span className="pill">
          <Ellipsis />
          {/* (phones reach the chat through More) */}
          {chatOn && unread > 0 && <span className="rail-badge mobile-only">{unread > 99 ? '99+' : unread}</span>}
        </span>
        <span className="label">{t('nav_others')}</span>
      </button>
      {menu && (
        <div className="menu more-sheet" ref={sheet}>
          <div className="sheet-head">
            <span>{t('more_title')}</span>
            <button className="icon-btn" onClick={() => setMenu(false)} aria-label="close">
              <X />
            </button>
          </div>
          <div className="sheet-grid mobile-only">
            <button disabled={!hasEpg} onClick={() => go('epg')}>
              <CalendarClock /> {t('nav_epg')}
            </button>
            <button onClick={() => go('spi')}>
              <FileCode2 /> {t('nav_spi')}
            </button>
            <button onClick={() => go('tii')}>
              <MapPinned /> {t('nav_tii')}
            </button>
            <button onClick={() => go('scanner')}>
              <ScanSearch /> {t('nav_scanner')}
            </button>
            <button onClick={() => go('signal')}>
              <AudioWaveform /> {t('nav_signal')}
            </button>
            {chatOn && (
              <button onClick={toggleChat}>
                <MessagesSquare /> {t('nav_chat')}
                {unread > 0 && <span className="rail-badge inline">{unread}</span>}
              </button>
            )}
            {fmPresent && linkingOn && (
              <button onClick={() => go('linking')}>
                <Link2 /> {t('nav_linking')}
              </button>
            )}
            <button onClick={() => go('settings')}>
              <SlidersHorizontal /> {t('nav_settings')}
            </button>
          </div>
          {admin && (
            <button onClick={() => go('stats')}>
              <Gauge /> {t('stats_title')}
            </button>
          )}
          {admin && (
            <button onClick={() => go('log')}>
              <ScrollText /> {t('log')}
            </button>
          )}
          <button
            className="desk-only"
            onClick={() => {
              set({ shortcutsOpen: true })
              setMenu(false)
            }}
          >
            <Keyboard /> {t('kb_title')}
          </button>
          <button onClick={() => go('about')}>
            <InfoIcon /> {t('about')}
          </button>
          {canUnlock && (
            <button
              onClick={() => {
                set({ tuneOpen: true })
                setMenu(false)
              }}
            >
              <LockKeyhole /> {t('tune_title')}
            </button>
          )}
          {admin ? (
            <button onClick={() => { logout(); setMenu(false) }}>
              <LogOut /> {t('logout')}
            </button>
          ) : (
            <button
              onClick={() => {
                set({ loginOpen: true })
                setMenu(false)
              }}
            >
              <LogIn /> {t('login')}
            </button>
          )}
        </div>
      )}
    </nav>
  )
}

/** The computer is too slow for the receiver: gaps in the sound, a signal that looks worse than it is. */
function OverloadBanner() {
  const t = useT()
  const o = useStore((s) => s.state?.overload)
  if (!o) return null
  const lost = o.lost < 10 ? o.lost.toFixed(1) : Math.round(o.lost)
  return <div className="banner">{(o.cpu != null ? t('overload_cpu').replace('{cpu}', String(o.cpu)) : t('overload')).replace('{lost}', String(lost))}</div>
}

/** The DTR-100 reports reception as 0..4 bars (its chip's error rate). */
function dongleLevel(s: Signal): 0 | 1 | 2 | 3 {
  if (!s.synced) return 0
  return Math.max(1, Math.min(3, s.bars ?? 0)) as 1 | 2 | 3
}

function signalText(signal: Signal | null | undefined, t: (k: string) => string) {
  if (signal?.input === 'eti' && signal.synced) return 'ETI/EDI'
  // (no I/Q, so no SNR: the chip's own channel error rate, 0 = perfect)
  if (signal?.input === 'dongle' && signal.synced) return signal.cer != null ? `CER ${signal.cer}` : 'DTR-100'
  return signal?.synced ? `${signal.snr.toFixed(1)} dB` : signal?.signal ? t('searching') : t('no_signal')
}

/** Phone player bar: station, signal and a big play/stop button. */
function MiniPlayer() {
  const t = useT()
  const set = useStore((s) => s.set)
  const svc = useStore((s) => findService(s.ensemble, s.selected?.sid))
  const dls = useStore((s) => (s.selected ? s.services[s.selected.sid]?.dls?.text ?? s.dlsAll[s.selected.sid] : undefined))
  const signal = useStore((s) => s.signal)
  const audioEnabled = useStore((s) => s.audioEnabled)
  const audioPlaying = useStore((s) => s.audioPlaying)
  const lvl = signal ? (signal.input === 'eti' && signal.synced ? 3 : signal.input === 'dongle' ? dongleLevel(signal) : signalLevel(signal.synced, signal.signal, signal.snr, svc?.protection)) : 0
  return (
    <div className="miniplayer">
      <button className="mp-main" onClick={() => set({ view: 'service' })}>
        {svc ? <Logo label={svc.label} sid={svc.sid} small /> : <span className="mp-logo-empty" />}
        <span className="mp-txt">
          <span className="mp-name">{svc ? svc.label : t('no_service')}</span>
          <span className="mp-sub">
            <SignalBars level={lvl} synced={!!signal?.synced} />
            {signalText(signal, t)}
            {dls ? <span className="mp-dls"> · {dls}</span> : null}
          </span>
        </span>
      </button>
      <button className={'mp-play' + (audioEnabled && !audioPlaying ? ' buffering' : '')} onClick={toggleAudio} aria-label={audioEnabled ? t('stop') : t('play')}>
        {audioEnabled ? <Square /> : <Play />}
      </button>
    </div>
  )
}

function Clock() {
  const lang = useStore((s) => s.lang)
  const time = useStore((s) => s.time)
  const [, force] = useState(0)
  useEffect(() => {
    const i = setInterval(() => force((x) => x + 1), 1000)
    return () => clearInterval(i)
  }, [])
  if (time.utc === null) return <span />
  // DAB time (UTC, minute resolution) advanced by local elapsed time, shown in ensemble local time
  const utc = time.utc + (Date.now() - time.at) / 1000
  const d = new Date((utc + time.lto * 1800) * 1000)
  const pad = (n: number) => n.toString().padStart(2, '0')
  const wd = weekday(lang, d.getUTCDay())
  return (
    <span>
      {wd.charAt(0).toUpperCase() + wd.slice(1)}, {pad(d.getUTCDate())}.{pad(d.getUTCMonth() + 1)}.{d.getUTCFullYear()},{' '}
      {pad(d.getUTCHours())}:{pad(d.getUTCMinutes())}
    </span>
  )
}

/** Round trip of a ping to the server (see ws.ts), coloured when slow. */
function Latency() {
  const t = useT()
  const l = useStore((s) => s.latency)
  if (!l) return null
  const level = l.late || l.ms > 400 ? 'bad' : l.ms > 150 ? 'mid' : ''
  const value = l.ms >= 1000 ? `${(l.ms / 1000).toFixed(1)} s` : `${l.ms} ms`
  return (
    <span className={'latency ' + level} title={t(l.late ? 'latency_late' : 'latency_help')}>
      <Activity size={15} /> {l.late ? '> ' : ''}
      {value}
    </span>
  )
}

/**
 * The background picture of the server (Settings > Webserver), once it has
 * loaded: drawn darkened, the surfaces over it become translucent.
 */
function Background() {
  const url = useStore((s) => s.hello?.backgroundImage)
  const show = useStore((s) => s.showBackground)
  const [loaded, setLoaded] = useState<string | null>(null)
  useEffect(() => {
    if (!url || !show) return
    const img = new Image()
    img.onload = () => setLoaded(url)
    img.onerror = () => setLoaded(null)
    img.src = url
    return () => {
      img.onload = img.onerror = null
    }
  }, [url, show])
  const on = !!url && show && loaded === url
  useEffect(() => {
    if (on) document.documentElement.dataset.bg = ''
    else delete document.documentElement.dataset.bg
  }, [on])
  if (!on) return null
  return <div className="app-bg" aria-hidden style={{ backgroundImage: `url(${JSON.stringify(url)})` }} />
}

/** A new installation not set up yet: the admin logs in to do it. */
function SetupBanner() {
  const t = useT()
  const pending = useStore((s) => !!s.hello?.setupPending)
  const admin = useStore((s) => s.admin)
  const set = useStore((s) => s.set)
  // (the window of the Windows application logs in by itself)
  if (!pending || admin || desktop) return null
  return (
    <div className="banner update">
      {t('setup_banner')}{' '}
      <button className="btn primary" onClick={() => set({ loginOpen: true })}>
        {t('login')}
      </button>
    </div>
  )
}

function StatusBar() {
  const t = useT()
  const signal = useStore((s) => s.signal)
  const volume = useStore((s) => s.volume)
  const muted = useStore((s) => s.muted)
  const audioEnabled = useStore((s) => s.audioEnabled)
  const listeners = useStore((s) => s.listeners)
  const admin = useStore((s) => s.admin)
  const protection = useStore((s) => findService(s.ensemble, s.selected?.sid)?.protection)
  const ensLabel = useStore((s) => (s.ensemble.valid ? `${s.ensemble.label} · ${s.ensemble.channel}` : s.state?.channel ?? ''))
  const lvl = signal ? (signal.input === 'eti' && signal.synced ? 3 : signal.input === 'dongle' ? dongleLevel(signal) : signalLevel(signal.synced, signal.signal, signal.snr, protection)) : 0
  return (
    <footer className="statusbar">
      <SignalBars level={lvl} synced={!!signal?.synced} />
      <span title={signal?.input === 'dongle' ? 'DTR-100' : t('snr')}>{signalText(signal, t)}</span>
      <span className="ens-chip">{ensLabel}</span>
      <div className="center">
        <Clock />
      </div>
      <Latency />
      {admin ? (
        // the admin opens the list (kick, ban)
        <button className="icon-btn listeners-btn" title={t('listeners')} onClick={() => useStore.getState().set({ listenersOpen: true })}>
          <Users size={15} /> {listeners}
        </button>
      ) : (
        <span className="muted" title={t('listeners')} style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}>
          <Users size={15} /> {listeners}
        </span>
      )}
      <button className="icon-btn" onClick={toggleAudio} title={audioEnabled ? t('stop') : t('play')}>
        {audioEnabled ? <Square /> : <Play />}
      </button>
      <button className="icon-btn" onClick={toggleMute} title={t('mute')}>
        {muted ? <VolumeX /> : <Volume2 />}
      </button>
      <input
        className="volume"
        type="range"
        min={0}
        max={1}
        step={0.01}
        value={muted ? 0 : volume}
        onChange={(e) => setVolume(parseFloat(e.target.value))}
        title={t('volume')}
      />
    </footer>
  )
}

function Notices() {
  const t = useT()
  const notices = useStore((s) => s.notices)
  return (
    <div className="notices">
      {notices.map((n) => (
        <div key={n.id} className={'notice ' + n.level}>
          {n.message.startsWith('txdb_imported:')
            ? `${t('transmitters')}: ${n.message.split(':')[1]}`
            : t(n.message)}
        </div>
      ))}
    </div>
  )
}

function ScanBanner() {
  const t = useT()
  const scan = useStore((s) => s.state?.scan)
  if (!scan) return null
  return (
    <div className="scan-banner">
      <span>
        {t('scan_running')} {scan.channel} ({scan.index + 1}/{scan.total}) · {t('found')}: {scan.found}
      </span>
      <div className="progress">
        <div style={{ width: `${((scan.index + 1) / scan.total) * 100}%` }} />
      </div>
      <button className="btn" onClick={stopScan}>
        {t('stop_scan')}
      </button>
    </div>
  )
}

/**
 * Name and description of the receiver (Settings > Webserver) above every view,
 * like the tuner name and description of FM-DX Webserver; a long description
 * opens with a click.
 */
function StationHeader() {
  const name = useStore((s) => s.hello?.stationName)
  const description = useStore((s) => s.hello?.description)
  const [open, setOpen] = useState(false)
  const named = !!name && name !== 'MagicDAB'
  if (!named && !description) return null
  return (
    <div
      className={'station-head' + (open ? ' open' : '') + (description ? ' has-desc' : '')}
      onClick={(e) => {
        if (description && !(e.target as HTMLElement).closest('a')) setOpen(!open)
      }}
    >
      <RadioReceiver size={16} className="station-icon" />
      {named && <b className="station-name">{name}</b>}
      {description && (
        <span className="station-desc">
          <Markdown text={open ? description : description.split(/\n+/).join('  ·  ')} />
        </span>
      )}
    </div>
  )
}

/** A newer version of the program, for admins (one click to install). */
function UpdateBanner() {
  const t = useT()
  const admin = useStore((s) => s.admin)
  const u = useStore((s) => s.update)
  const [later, setLater] = useState<string | null>(() => {
    try {
      return sessionStorage.getItem('magicdab-update-later')
    } catch {
      return null
    }
  })
  const v = u?.available?.version
  if (!admin || !u || !v || later === v) return null
  return (
    <div className="banner update">
      {u.busy ? t('upd_busy') : t('upd_banner').replace('{v}', v)}{' '}
      <button className="btn primary" disabled={u.busy} onClick={() => send({ type: 'updateInstall' })}>
        {t('upd_install')}
      </button>{' '}
      <button
        className="btn"
        disabled={u.busy}
        onClick={() => {
          setLater(v)
          try {
            sessionStorage.setItem('magicdab-update-later', v)
          } catch {
            /* storage unavailable */
          }
        }}
      >
        {t('upd_later')}
      </button>
    </div>
  )
}

export default function App() {
  useThemeEffect()
  const t = useT()
  const view = useStore((s) => s.view)
  const chatOpen = useStore((s) => s.chatOpen)
  const connection = useStore((s) => s.connection)
  const closedBy = useStore((s) => s.closedBy)
  const uiUpdate = useStore((s) => s.uiUpdate)
  const station = useStore((s) => s.hello?.stationName)
  const svcLabel = useStore((s) => findService(s.ensemble, s.selected?.sid)?.label)
  // a private server and this browser gave no password (the program's own
  // window logs in with its token)
  const gated = useStore((s) => !!s.hello?.private && !s.admin && !s.tuner && !desktopToken)

  useEffect(() => {
    document.title = svcLabel ? `${svcLabel} – ${station ?? 'MagicDAB'}` : station ?? 'MagicDAB'
  }, [svcLabel, station])

  useEffect(() => {
    setSubscriptions({ spectrum: view === 'signal', tiiPairs: view === 'tii', log: view === 'log', linking: view === 'linking', serverStats: view === 'stats' })
  }, [view])

  // linking is the admin's: logged out (or never in) its view goes away
  const admin = useStore((s) => s.admin)
  useEffect(() => {
    if (view === 'linking' && !admin) useStore.getState().set({ view: 'service' })
  }, [view, admin])

  if (gated) {
    return (
      <div className="app">
        <PrivateGate />
        <Notices />
      </div>
    )
  }

  return (
    <div className="app">
      <Background />
      <NavRail />
      <main className="main">
        <StationHeader />
        {connection === 'closed' && !closedBy && <div className="banner">{t('connection_lost')}</div>}
        {closedBy && (
          <div className="banner">
            {t(closedBy === 'kicked' ? 'kicked' : closedBy === 'idle' ? 'idle_closed' : 'refused')}{' '}
            {closedBy !== 'refused' && (
              <button className="btn" onClick={reconnectAfterKick}>
                {t('reconnect')}
              </button>
            )}
          </div>
        )}
        <SetupBanner />
        {uiUpdate && (
          <div className="banner update">
            {t('ui_update')}{' '}
            <button className="btn" onClick={() => location.reload()}>
              {t('reload')}
            </button>
          </div>
        )}
        <UpdateBanner />
        <ScanBanner />
        <OverloadBanner />
        {view === 'service' && <ServiceView />}
        {view === 'stations' && <StationsView />}
        {view === 'linking' && admin && <LinkingView />}
        {view === 'spi' && <SpiView />}
        {view === 'info' && <InfoView />}
        {view === 'tii' && <TiiView />}
        {view === 'signal' && <SignalView />}
        {view === 'scanner' && <ScannerView />}
        {view === 'settings' && <SettingsView />}
        {view === 'log' && <LogView />}
        {view === 'stats' && <StatsView />}
        {view === 'about' && <AboutView />}
        {view === 'epg' && <GuideView />}
        {view === 'catsls' && <SlidesView />}
      </main>
      <StatusBar />
      <MiniPlayer />
      <Notices />
      <LogoDialog />
      {chatOpen && <ChatPopup />}
      <DesktopClose />
      <LoginDialog />
      <TuneDialog />
      <ListenersDialog />
      <Shortcuts />
      <SetupWizard />
      <StillHere />
    </div>
  )
}
