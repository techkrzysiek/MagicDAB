import { useEffect, useRef, useState } from 'react'
import L from 'leaflet'
import { Eye, EyeOff, LocateFixed, WandSparkles, X } from 'lucide-react'
import { useStore, save, sessionGet, sessionSet, type Lang, type Theme } from '../store'
import { useT } from '../i18n'
import { saveSettings } from '../actions'
import { send } from '../ws'
import { desktop } from '../desktop'

const STEPS = ['welcome', 'station', 'location', 'device', 'password', 'tunnel', 'summary'] as const
type Step = (typeof STEPS)[number]

/** "lat, lon" typed in (also "52,2297 21,0122" or "52,2297; 21,0122" with decimal commas), or null. */
export function parseLocation(s: string): { lat: number; lon: number } | null {
  const v = s.replace(/°/g, '').trim()
  const parts = v.includes(';') ? v.split(/\s*;\s*/) : /^-?\d+,\d+\s+-?\d+,\d+$/.test(v) ? v.split(/\s+/) : v.split(/[,\s]+/)
  const [lat, lon, ...rest] = parts.map((x) => (x.trim() ? Number(x.trim().replace(',', '.')) : NaN))
  if (rest.length || !isFinite(lat) || !isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null
  return { lat, lon }
}

export const fmtLocation = (lat: number, lon: number) => `${lat.toFixed(5)}, ${lon.toFixed(5)}`

/** Click on the map to choose the location. */
export function LocationMap({ value, onPick }: { value: { lat: number; lon: number } | null; onPick: (lat: number, lon: number) => void }) {
  const div = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const marker = useRef<L.CircleMarker | null>(null)
  const pick = useRef(onPick)
  pick.current = onPick
  // (a click moves the marker only; typed coordinates are shown up close)
  const clicked = useRef(false)
  useEffect(() => {
    if (!div.current) return
    // (no zoom animation: a map removed in the middle of one, by the next
    // step, throws in Leaflet)
    const m = L.map(div.current, { zoomControl: true, attributionControl: true, zoomAnimation: false }).setView(
      value ? [value.lat, value.lon] : [51.9, 19.1],
      value ? 9 : 5,
    )
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 18,
      attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
    }).addTo(m)
    m.on('click', (e: L.LeafletMouseEvent) => {
      clicked.current = true
      pick.current(e.latlng.lat, e.latlng.lng)
    })
    map.current = m
    setTimeout(() => m.invalidateSize(), 50)
    return () => {
      m.remove()
      map.current = null
      marker.current = null
    }
  }, [])
  useEffect(() => {
    const m = map.current
    if (!m) return
    if (!value) {
      marker.current?.remove()
      marker.current = null
      return
    }
    if (marker.current) marker.current.setLatLng([value.lat, value.lon])
    else marker.current = L.circleMarker([value.lat, value.lon], { radius: 7, color: '#ffffff', fillColor: '#6d3fd8', fillOpacity: 1, weight: 3 }).addTo(m)
    if (!clicked.current) m.setView([value.lat, value.lon], Math.max(m.getZoom(), 9))
    clicked.current = false
  }, [value?.lat, value?.lon])
  return <div ref={div} className="setup-map" />
}

/**
 * First-run wizard: walks the admin through the settings that matter on a new
 * installation (the window of the Windows application is admin by itself;
 * over the network the generated password is needed). Everything is saved at
 * the end with the usual settings command.
 */
export function SetupWizard() {
  const open = useStore((s) => s.setupOpen)
  const admin = useStore((s) => s.admin)
  const pending = useStore((s) => !!s.hello?.setupPending)
  const haveSettings = useStore((s) => !!s.settings?.settings)
  const set = useStore((s) => s.set)

  // a new installation: open by itself for the admin (unless put off in this session)
  useEffect(() => {
    if (pending && admin && haveSettings && !sessionGet('magicdab.setupLater')) set({ setupOpen: true })
  }, [pending, admin, haveSettings, set])
  // (logged out meanwhile: the settings are not for others)
  useEffect(() => {
    if (open && !admin) set({ setupOpen: false })
  }, [open, admin, set])

  if (!open || !admin || !haveSettings) return null
  return <Wizard />
}

function Wizard() {
  const t = useT()
  const set = useStore((s) => s.set)
  const lang = useStore((s) => s.lang)
  const theme = useStore((s) => s.theme)
  const devices: string[] = useStore((s) => s.settings?.rtlsdrDevices) ?? []
  const passwordFromEnv = useStore((s) => !!s.settings?.passwordFromEnv)
  const deviceFromArgs = useStore((s) => !!s.settings?.deviceFromArgs)
  const emptyList = useStore((s) => s.serviceList.length === 0)
  // (sent while disconnected, the settings would be lost)
  const online = useStore((s) => s.connection === 'open')
  // the form starts from the settings as they are when the wizard opens
  const [initial] = useState(() => useStore.getState().settings.settings)
  const [step, setStep] = useState<Step>('welcome')
  const [name, setName] = useState(initial.stationName === 'MagicDAB' ? '' : initial.stationName ?? '')
  const [description, setDescription] = useState(initial.description ?? '')
  const [loc, setLoc] = useState(initial.location ? fmtLocation(initial.location.lat, initial.location.lon) : '')
  const [geo, setGeo] = useState<{ busy: boolean; error: string | null }>({ busy: false, error: null })
  const [dev, setDev] = useState<any>(initial.device ?? { type: 'rtlSdr', index: 0, ppm: 0, bias_tee: false })
  const [pw, setPw] = useState('')
  const [pw2, setPw2] = useState('')
  const [showPw, setShowPw] = useState(false)
  const [tunnel, setTunnel] = useState<any>({ host: 'dxserver.eu', port: 7000, username: '', subdomain: '', token: '', ...initial.tunnel })
  const [scan, setScan] = useState(emptyList)
  const body = useRef<HTMLDivElement>(null)

  const index = STEPS.indexOf(step)
  const location = parseLocation(loc)
  const pwError = pw && pw.length < 4 ? 'setup_password_short' : pw && pw2 !== pw ? 'setup_password_mismatch' : null
  const devError = dev.type === 'rtlTcp' && (!String(dev.host ?? '').trim() || !(dev.port > 0 && dev.port < 65536))
  const tunnelUrl = tunnel.enabled && tunnel.subdomain && tunnel.host ? `https://${tunnel.subdomain}.${tunnel.host}` : null
  const blocked =
    (step === 'location' && loc.trim() !== '' && !location) ||
    (step === 'device' && devError) ||
    (step === 'password' && !!pwError) ||
    // (the server starts a tunnel with both only)
    (step === 'tunnel' && tunnel.enabled && !tunnelUrl)

  useEffect(() => {
    // the first field of a step, ready for typing
    body.current?.querySelector<HTMLElement>('input[type=text], input[type=password], textarea')?.focus()
  }, [step])

  const later = () => {
    sessionSet('magicdab.setupLater', '1')
    set({ setupOpen: false })
  }
  const skip = () => {
    saveSettings({ setupPending: false })
    set({ setupOpen: false })
  }
  const finish = () => {
    const s: Record<string, unknown> = {
      stationName: name.trim() || 'MagicDAB',
      description: description.trim(),
      setupPending: false,
    }
    if (location) s.location = location
    if (JSON.stringify(dev) !== JSON.stringify(initial.device)) {
      s.device = dev.type === 'rtlTcp' ? { ...dev, host: String(dev.host).trim(), port: Number(dev.port) } : dev
    }
    if (JSON.stringify(tunnel) !== JSON.stringify({ host: 'dxserver.eu', port: 7000, username: '', subdomain: '', token: '', ...initial.tunnel })) {
      s.tunnel = tunnel
    }
    if (pw) s.adminPassword = pw
    saveSettings(s)
    // reconnects log in with it (as after a change in the settings)
    if (pw && !desktop) sessionSet('magicdab.pw', pw)
    // (after the settings: on the new input)
    if (scan && (dev.type === 'rtlSdr' || dev.type === 'dtr100')) send({ type: 'scan', action: 'start' })
    set({ setupOpen: false })
  }
  const last = index === STEPS.length - 1
  const canGo = !blocked && (!last || online)
  const next = () => {
    if (!canGo) return
    if (!last) setStep(STEPS[index + 1])
    else finish()
  }

  const locateDevice = () => {
    if (!navigator.geolocation) return
    setGeo({ busy: true, error: null })
    navigator.geolocation.getCurrentPosition(
      (p) => {
        setLoc(fmtLocation(p.coords.latitude, p.coords.longitude))
        setGeo({ busy: false, error: null })
      },
      (e) => setGeo({ busy: false, error: e.message || String(e.code) }),
      { enableHighAccuracy: true, timeout: 20000, maximumAge: 600000 },
    )
  }

  const devLabel =
    dev.type === 'rtlSdr'
      ? `RTL-SDR ${devices[dev.index] ?? '#' + dev.index}`
      : dev.type === 'rtlTcp'
        ? `rtl_tcp ${dev.host}:${dev.port}`
        : dev.type === 'none'
          ? t('setup_dev_none')
          : dev.type === 'dtr100'
            ? 'DTR-100'
            : dev.path ?? dev.type

  return (
    <div className="modal-back">
      <form
        className="modal setup-modal"
        role="dialog"
        aria-label={t('setup_title')}
        onSubmit={(e) => {
          e.preventDefault()
          next()
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape') later()
        }}
      >
        <div className="modal-head">
          <WandSparkles size={18} />
          <b>{t('setup_title')}</b>
          <span style={{ flex: 1 }} />
          <button type="button" className="icon-btn" onClick={later} title={t('setup_later')} aria-label={t('setup_later')}>
            <X />
          </button>
        </div>
        <ol className="setup-steps">
          {STEPS.map((s, i) => (
            <li key={s} className={(i === index ? 'on' : '') + (i < index ? ' done' : '')}>
              <button type="button" disabled={i > index} onClick={() => setStep(s)}>
                <span className="dot">{i + 1}</span>
                <span className="name">{t('setup_step_' + s)}</span>
              </button>
            </li>
          ))}
        </ol>
        <div className="setup-body" ref={body}>
          {step === 'welcome' && (
            <>
              <h2>{t('setup_welcome')}</h2>
              <p>{t('setup_welcome_text')}</p>
              <div className="setup-choice">
                {(['pl', 'en'] as Lang[]).map((l) => (
                  <button
                    type="button"
                    key={l}
                    className={'btn' + (lang === l ? ' primary' : '')}
                    onClick={() => {
                      set({ lang: l })
                      save('lang', l)
                    }}
                  >
                    {l === 'pl' ? 'Polski' : 'English'}
                  </button>
                ))}
                <select
                  value={theme}
                  aria-label={t('theme')}
                  onChange={(e) => {
                    set({ theme: e.target.value as Theme })
                    save('theme', e.target.value)
                  }}
                >
                  <option value="auto">{t('theme')}: {t('theme_auto')}</option>
                  <option value="light">{t('theme')}: {t('theme_light')}</option>
                  <option value="dark">{t('theme')}: {t('theme_dark')}</option>
                </select>
              </div>
              <p className="hint">{t('setup_lang_help')}</p>
            </>
          )}
          {step === 'station' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_station_help')}</p>
              <label>
                {t('station_name')}
                <input type="text" value={name} maxLength={80} placeholder="MagicDAB" onChange={(e) => setName(e.target.value)} />
              </label>
              <label>
                {t('server_description')}
                <textarea rows={4} value={description} placeholder={t('server_description_help')} onChange={(e) => setDescription(e.target.value)} />
              </label>
            </div>
          )}
          {step === 'location' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_location_help')}</p>
              <label>
                {t('rx_location')}
                <span className="row">
                  <input
                    type="text"
                    className={loc.trim() && !location ? 'invalid' : ''}
                    value={loc}
                    placeholder="52.2297, 21.0122"
                    onChange={(e) => setLoc(e.target.value)}
                  />
                  <button type="button" className="btn" disabled={!window.isSecureContext || !navigator.geolocation || geo.busy} onClick={locateDevice}>
                    <LocateFixed /> {geo.busy ? '…' : t('setup_geo')}
                  </button>
                </span>
              </label>
              {loc.trim() !== '' && !location && <div className="hint error">{t('setup_location_invalid')}</div>}
              {!window.isSecureContext && <div className="hint">{t('setup_geo_unavailable')}</div>}
              {geo.error && <div className="hint error">{t('setup_geo_error').replace('{e}', geo.error)}</div>}
              <LocationMap value={location} onPick={(lat, lon) => setLoc(fmtLocation(lat, lon))} />
              <div className="hint">{t('setup_map_help')}</div>
            </div>
          )}
          {step === 'device' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_device_help')}</p>
              <div className="setup-options" role="radiogroup">
                {(['rtlSdr', 'rtlTcp', 'dtr100', 'none'] as const).map((type) => (
                  <label key={type} className={'setup-option' + (dev.type === type ? ' on' : '')}>
                    <input
                      type="radio"
                      name="setup-device"
                      checked={dev.type === type}
                      onChange={() => {
                        // (back to the input of the settings: as it was)
                        if (type === initial.device?.type) setDev(initial.device)
                        else if (type === 'rtlSdr') setDev({ type, index: 0, ppm: 0, bias_tee: false })
                        else if (type === 'rtlTcp') setDev({ type, host: '', port: 1234 })
                        else setDev({ type })
                      }}
                    />
                    <span>
                      <b>{t('setup_dev_' + type.toLowerCase())}</b>
                      <span className="hint">{t('setup_dev_' + type.toLowerCase() + '_help')}</span>
                    </span>
                  </label>
                ))}
              </div>
              {dev.type === 'rtlSdr' &&
                (devices.length ? (
                  <label>
                    {t('rtl_index')}
                    <select value={dev.index} onChange={(e) => setDev({ ...dev, index: parseInt(e.target.value) })}>
                      {devices.map((d, i) => (
                        <option key={i} value={i}>
                          {d}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : (
                  <div className="hint">{t('setup_dev_none_found')}</div>
                ))}
              {dev.type === 'rtlTcp' && (
                <label>
                  {t('host_port')}
                  <span className="row">
                    <input type="text" value={dev.host} placeholder="192.168.1.10" onChange={(e) => setDev({ ...dev, host: e.target.value })} />
                    <input
                      type="number"
                      min={1}
                      max={65535}
                      value={dev.port}
                      style={{ width: 100 }}
                      onChange={(e) => setDev({ ...dev, port: parseInt(e.target.value) || 0 })}
                    />
                  </span>
                </label>
              )}
              {deviceFromArgs && <div className="hint warn">{t('setup_dev_args')}</div>}
            </div>
          )}
          {step === 'password' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_password_help')}</p>
              {desktop && <div className="hint">{t('setup_password_desktop')}</div>}
              {passwordFromEnv && <div className="hint warn">{t('setup_password_env')}</div>}
              <label>
                {t('new_password')}
                <span className="pw-field">
                  <input type={showPw ? 'text' : 'password'} autoComplete="new-password" value={pw} onChange={(e) => setPw(e.target.value)} />
                  <button type="button" className="icon-btn" onClick={() => setShowPw(!showPw)} aria-label={showPw ? 'hide' : 'show'}>
                    {showPw ? <EyeOff size={18} /> : <Eye size={18} />}
                  </button>
                </span>
              </label>
              <label>
                {t('setup_password_repeat')}
                <input type={showPw ? 'text' : 'password'} autoComplete="new-password" value={pw2} disabled={!pw} onChange={(e) => setPw2(e.target.value)} />
              </label>
              {pwError && (pw.length < 4 || pw2) && <div className="hint error">{t(pwError)}</div>}
            </div>
          )}
          {step === 'tunnel' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_tunnel_help')}</p>
              <label className="check">
                <input type="checkbox" checked={!!tunnel.enabled} onChange={(e) => setTunnel({ ...tunnel, enabled: e.target.checked })} /> {t('tunnel_enable')}
              </label>
              {tunnel.enabled && (
                <>
                  <label>
                    {t('tunnel_host')}
                    <span className="row">
                      <input type="text" value={tunnel.host} placeholder="dxserver.eu" onChange={(e) => setTunnel({ ...tunnel, host: e.target.value.trim() })} />
                      <input
                        type="number"
                        value={tunnel.port}
                        style={{ width: 100 }}
                        onChange={(e) => setTunnel({ ...tunnel, port: parseInt(e.target.value) || 7000 })}
                      />
                    </span>
                  </label>
                  <label>
                    {t('tunnel_user')}
                    <input type="text" value={tunnel.username} onChange={(e) => setTunnel({ ...tunnel, username: e.target.value.trim() })} />
                  </label>
                  <label>
                    {t('tunnel_subdomain')}
                    <input type="text" value={tunnel.subdomain} onChange={(e) => setTunnel({ ...tunnel, subdomain: e.target.value.trim() })} />
                  </label>
                  <label>
                    {t('tunnel_token')}
                    <input type="password" autoComplete="off" value={tunnel.token} onChange={(e) => setTunnel({ ...tunnel, token: e.target.value.trim() })} />
                  </label>
                  {tunnelUrl && <div className="hint">{tunnelUrl}</div>}
                </>
              )}
            </div>
          )}
          {step === 'summary' && (
            <div className="setup-fields">
              <p className="hint">{t('setup_summary_help')}</p>
              <dl className="setup-summary">
                <dt>{t('station_name')}</dt>
                <dd>{name.trim() || 'MagicDAB'}</dd>
                <dt>{t('setup_step_location')}</dt>
                <dd>{location ? fmtLocation(location.lat, location.lon) : t('setup_not_set')}</dd>
                <dt>{t('device')}</dt>
                <dd>{devLabel}</dd>
                <dt>{t('setup_step_password')}</dt>
                <dd>{pw ? t('setup_pw_changed') : t('setup_pw_kept')}</dd>
                <dt>{t('tunnel')}</dt>
                <dd>{tunnelUrl ?? t('tunnel_state_off')}</dd>
              </dl>
              {/* (rtl_tcp stays on one channel: nothing to scan) */}
              {dev.type === 'rtlSdr' && (
                <label className="check">
                  <input type="checkbox" checked={scan} onChange={(e) => setScan(e.target.checked)} /> {t('setup_scan')}
                </label>
              )}
            </div>
          )}
        </div>
        <div className="setup-actions">
          <button type="button" className="btn link" disabled={!online} onClick={skip}>
            {t('setup_skip')}
          </button>
          <span style={{ flex: 1 }} />
          {index > 0 && (
            <button type="button" className="btn" onClick={() => setStep(STEPS[index - 1])}>
              {t('setup_back')}
            </button>
          )}
          <button type="submit" className="btn primary" disabled={!canGo}>
            {last ? t('setup_finish') : t('setup_next')}
          </button>
        </div>
      </form>
    </div>
  )
}
