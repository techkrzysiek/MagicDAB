import { useEffect, useRef, useState } from 'react'
import { androidApp, closePrefs, desktop, desktopServerMode, installApk, toHost } from '../desktop'
import { useStore, save, isLocal } from '../store'
import type { ServerMapStatus } from '../store'
import { useT } from '../i18n'
import { ViewHeader } from '../components/common'
import { importTxDb, login, logout, saveSettings, setBuffer as applyBuffer, setCodec, setNativeAudio } from '../actions'
import { send } from '../ws'
import { defaultBuffer } from '../audio/player'
import { Card, Choice, Row, SaveBar, Toggle, fieldOk, rule, useDraft, useField } from '../components/SettingsKit'
import { LocationMap, parseLocation } from '../components/SetupWizard'
import AccessSettings from './AccessSettings'

/** Stable fallback for store selectors (a new [] each time loops with zustand 5). */
const EMPTY: never[] = []

type Tab = 'general' | 'station' | 'receiver' | 'fm' | 'tii' | 'internet' | 'access' | 'tunnel'

/** Tabs of the server settings (admins), in this order. */
const SERVER_TABS: Tab[] = ['station', 'receiver', 'fm', 'tii', 'internet', 'access', 'tunnel']
/** Not in the Android app: its server is for the phone only (Settings::phone_only). */
const PUBLIC_TABS: Tab[] = ['access', 'tunnel', 'fm']

/** Where each field of the draft is (a mark on the tab with changes). */
const TAB_OF: Record<string, Tab> = {
  stationName: 'station', description: 'station', location: 'station', backgroundImage: 'station', serverMap: 'station', sourceUrl: 'station', openBrowser: 'station',
  device: 'receiver', ppm: 'receiver', lockChannel: 'receiver', lockService: 'receiver', channelMaps: 'receiver', etiDelayMs: 'receiver', listenerMultiplex: 'receiver', listenerMultiplexLimit: 'receiver', idleScan: 'receiver', maxServices: 'receiver', streams: 'receiver', backgroundPad: 'receiver', spi: 'receiver', opusBitrate: 'receiver',
  fmEnabled: 'fm', fmPort: 'fm', fmAudio: 'fm', fmAntennas: 'fm', linkingEnabled: 'fm', linkingMatchLevel: 'fm',
  tiiMode: 'tii', tiiLog: 'tii',
  internet: 'internet', radioVis: 'internet', discordWebhook: 'internet', chatEnabled: 'internet', noIndex: 'internet', updateUrl: 'internet', updateAuto: 'internet',
  publicServer: 'access', tunerLocked: 'access', tunePassword: 'access', allowedChannels: 'access', blockTor: 'access', blockedAsns: 'access', autoBanHours: 'access', adminPassword: 'access', idleCheck: 'access',
  tunnel: 'tunnel',
}

// a stream (host:port) or a recording on the server (file:///path)
const validMap = (u: string) => /^(edi\+tcp|eti\+tcp|edi|eti|tcp):\/\/[^/]+:\d+\/?$/i.test(u.trim()) || /^file:\/*[^\s/][^\s]*$/i.test(u.trim())
const locText = (l?: { lat: number; lon: number } | null) => (l ? `${l.lat}, ${l.lon}` : '')
const webAddress = /^https?:\/\/[^\s"'\\<>`]+$/i
// a picture uploaded to the server (background.rs)
const uploadedPicture = /^\/background\/[0-9a-f]{32}\.(jpg|png|webp)$/

rule('stationName', { ok: (v: string) => !!v.trim(), encode: (v: string) => v.trim() })
rule('location', { ok: (v: string) => !v.trim() || !!parseLocation(v), encode: (v: string) => parseLocation(v) })
rule('backgroundImage', { ok: (v: string) => !v.trim() || webAddress.test(v.trim()) || uploadedPicture.test(v.trim()), encode: (v: string) => v.trim() })
rule('sourceUrl', { ok: (v: string) => !v.trim() || webAddress.test(v.trim()), encode: (v: string) => v.trim() })
rule('channelMaps', {
  ok: (v: { url: string }[]) => v.every((m) => validMap(m.url)),
  encode: (v: { channel: string; url: string }[]) => v.map((m) => ({ channel: m.channel, url: m.url.trim() })),
})
// (empty: the one saved stays; null: removed)
rule('discordWebhook', {
  ok: (v: string | null) => v === null || !v.trim() || /^https:\/\/([a-z]+\.)?discord(app)?\.com\/api\/webhooks\/\S+$/i.test(v.trim()),
  encode: (v: string | null) => (v === null ? null : v.trim()),
})
rule('updateUrl', { ok: (v: string) => !v.trim() || /^https:\/\/\S+$/i.test(v.trim()), encode: (v: string) => v.trim() })
rule('tunnel', { ok: (f: any) => !f.enabled || (!!f.host?.trim() && !!f.subdomain?.trim() && f.port > 0 && f.port < 65536) })
rule('serverMap', { ok: (f: any) => (!f.publicUrl.trim() || webAddress.test(f.publicUrl.trim())) && (!f.url.trim() || /^https:\/\/\S+$/i.test(f.url.trim())) })

// ------------------------------------------------------------------ this browser

function General() {
  const t = useT()
  const lang = useStore((s) => s.lang)
  const theme = useStore((s) => s.theme)
  const codec = useStore((s) => s.codec)
  const canNative = useStore((s) => !!s.hello?.speaker)
  const native = useStore((s) => s.nativeAudio)
  const speakerError = useStore((s) => s.speakerError)
  const set = useStore((s) => s.set)
  const admin = useStore((s) => s.admin)
  const hasBackground = useStore((s) => !!s.hello?.backgroundImage)
  const showBackground = useStore((s) => s.showBackground)
  const useRadioDns = useStore((s) => s.useRadioDns)
  const [buffer, setBuffer] = useState<number>(() => {
    try {
      return JSON.parse(localStorage.getItem('magicdab.buffer') ?? String(defaultBuffer()))
    } catch {
      return defaultBuffer()
    }
  })
  return (
    <>
      {!androidApp && <p className="set-intro hint">{t('set_local_note')}</p>}
      <Card title={t('set_card_ui')}>
        <Row label={t('language')}>
          <Choice
            value={lang}
            options={[
              ['pl', 'Polski'],
              ['en', 'English'],
            ]}
            onChange={(v) => {
              set({ lang: v })
              save('lang', v)
            }}
          />
        </Row>
        <Row label={t('theme')}>
          <Choice
            value={theme}
            options={[
              ['auto', t('theme_auto')],
              ['light', t('theme_light')],
              ['dark', t('theme_dark')],
            ]}
            onChange={(v) => {
              set({ theme: v })
              save('theme', v)
            }}
          />
        </Row>
        {hasBackground && (
          <Row label={t('bg_show')} hint={t('bg_show_hint')}>
            <Toggle
              checked={showBackground}
              onChange={(v) => {
                set({ showBackground: v })
                save('showBackground', v)
              }}
            />
          </Row>
        )}
        {!androidApp && (
          <Row label={t('kb_title')} hint={t('kb_hint_settings')}>
            <button className="btn" onClick={() => set({ shortcutsOpen: true })}>
              {t('kb_show')}
            </button>
          </Row>
        )}
      </Card>
      <Card title={t('set_card_audio')}>
        {canNative && (
          <Row label={t('audio_native')} hint={speakerError ? `${t('audio_native_error')}: ${speakerError}` : t('audio_native_hint')}>
            <Toggle checked={native} onChange={setNativeAudio} />
          </Row>
        )}
        {/* (the app plays PCM from its own server) */}
        {isLocal() && !androidApp && !(canNative && native) && (
          <Row label={t('audio_codec')}>
            <select value={codec} onChange={(e) => setCodec(e.target.value as 'opus' | 'pcm')}>
              <option value="opus">{t('codec_opus')}</option>
              <option value="pcm">{t('codec_pcm')}</option>
            </select>
          </Row>
        )}
        <Row label={t('buffer')} hint={t('set_buffer_hint')}>
          <div className="set-inline">
            <input
              type="range"
              min={0.1}
              max={2}
              step={0.05}
              value={buffer}
              onChange={(e) => {
                const v = parseFloat(e.target.value)
                setBuffer(v)
                applyBuffer(v)
              }}
            />
            <span className="set-value">{buffer.toFixed(2)} s</span>
          </div>
        </Row>
        <Row label={t('use_radiodns')} hint={t('radiodns_hint')}>
          <Toggle
            checked={useRadioDns}
            onChange={(v) => {
              set({ useRadioDns: v })
              save('useRadioDns', v)
            }}
          />
        </Row>
      </Card>
      {desktop && <DesktopPrefs />}
      {!admin && <AdminLogin />}
    </>
  )
}

/** The program's window on this computer (stored by the window, applied at once). */
function DesktopPrefs() {
  const t = useT()
  const openBrowser = useStore((s) => s.settings?.settings?.openBrowser ?? true)
  const [action, setAction] = useState(closePrefs.action)
  const [serverMode, setServerMode] = useState(desktopServerMode)
  return (
    <Card title={t('set_card_app')}>
      <Row label={t('close_action')}>
        <select
          value={action}
          onChange={(e) => {
            const v = e.target.value as 'ask' | 'hide' | 'quit' | 'server'
            closePrefs.action = v
            setAction(v)
          }}
        >
          <option value="ask">{t('close_ask')}</option>
          <option value="hide">{t('close_hide')}</option>
          <option value="server">{t('close_server')}</option>
          <option value="quit">{t('close_quit')}</option>
        </select>
      </Row>
      <Row label={t('server_mode')} hint={t('server_mode_hint')}>
        <Toggle
          checked={serverMode}
          onChange={(v) => {
            setServerMode(v)
            toHost({ type: 'serverMode', value: v })
          }}
        />
      </Row>
      <Row label={t('start_hidden')}>
        <Toggle checked={!openBrowser} onChange={(v) => saveSettings({ openBrowser: !v })} />
      </Row>
    </Card>
  )
}

function AdminLogin() {
  const t = useT()
  const [pw, setPw] = useState('')
  return (
    <Card title={t('set_card_admin')}>
      <Row label={t('password')} hint={t('set_login_hint')}>
        <div className="set-inline">
          <input type="password" autoComplete="current-password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && login(pw)} />
          <button className="btn primary" onClick={() => login(pw)}>
            {t('login')}
          </button>
        </div>
      </Row>
    </Card>
  )
}

// ------------------------------------------------------------------ station

function Station() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const windows = useStore((st) => st.update?.platform === 'windows-x64')
  // (null: not Windows, or not checked yet)
  const firewall = useStore((st) => st.settings?.firewall as boolean | null | undefined)
  const [name, setName] = useField('stationName', s?.stationName ?? '')
  const [desc, setDesc] = useField('description', s?.description ?? '')
  const [loc, setLoc] = useField('location', locText(s?.location))
  const [bg, setBg] = useField('backgroundImage', s?.backgroundImage ?? '')
  const [src, setSrc] = useField('sourceUrl', s?.sourceUrl ?? '')
  const [openBrowser, setOpenBrowser] = useField('openBrowser', s?.openBrowser ?? true)
  const [map, setMap] = useState(false)
  if (!s) return null
  const where = parseLocation(loc)
  return (
    <>
      <Card title={t('set_card_station')}>
        <Row label={t('station_name')} hint={t('set_station_name_hint')}>
          <input type="text" className={fieldOk('stationName', name) ? '' : 'invalid'} value={name} maxLength={80} onChange={(e) => setName(e.target.value)} />
        </Row>
        {!androidApp && (
          <Row label={t('server_description')} hint={t('server_description_help')} wide>
            <textarea rows={4} value={desc} onChange={(e) => setDesc(e.target.value)} />
          </Row>
        )}
        <Row label={t('rx_location')} hint={t('set_location_hint')}>
          <div className="set-inline">
            <input type="text" className={fieldOk('location', loc) ? '' : 'invalid'} value={loc} placeholder="52.2297, 21.0122" onChange={(e) => setLoc(e.target.value)} />
            <button className="btn" onClick={() => setMap(!map)}>
              {t(map ? 'set_map_hide' : 'set_map_pick')}
            </button>
          </div>
        </Row>
        {map && (
          <div className="set-row wide">
            <LocationMap value={where} onPick={(lat, lon) => setLoc(`${lat.toFixed(5)}, ${lon.toFixed(5)}`)} />
          </div>
        )}
        <Row label={t('bg_image')} hint={t('bg_image_help')} wide>
          <div className="set-inline">
            <input type="text" className={fieldOk('backgroundImage', bg) ? '' : 'invalid'} placeholder="https://…/background.jpg" value={bg} onChange={(e) => setBg(e.target.value)} />
            <BackgroundUpload onDone={setBg} />
            {bg && fieldOk('backgroundImage', bg) && <img className="set-thumb" src={bg.trim()} alt="" referrerPolicy="no-referrer" />}
          </div>
        </Row>
      </Card>
      {!androidApp && <ServerMap />}
      {!androidApp && (
      <Card title={t('set_card_setup')}>
        <Row label={t('setup_title')} hint={t('setup_open_help')}>
          <button className="btn" onClick={() => useStore.getState().set({ setupOpen: true })}>
            {t('setup_open')}
          </button>
        </Row>
        <Row label={t('source_url')} hint={t('set_source_hint')}>
          <input type="text" className={fieldOk('sourceUrl', src) ? '' : 'invalid'} placeholder="https://github.com/…" value={src} onChange={(e) => setSrc(e.target.value)} />
        </Row>
        {windows && !desktop && (
          <Row label={t('no_browser_start')}>
            <Toggle checked={!openBrowser} onChange={(v) => setOpenBrowser(!v)} />
          </Row>
        )}
        {firewall != null && (
          <Row label={t('firewall')} hint={firewall ? t('firewall_ok') : t('firewall_hint')}>
            {firewall ? (
              <span className="set-value">{t('firewall_on')}</span>
            ) : (
              <button className="btn" onClick={() => send({ type: 'firewall' })}>
                {t('firewall_allow')}
              </button>
            )}
          </Row>
        )}
      </Card>
      )}
    </>
  )
}

/**
 * A background picture from this computer: uploaded to the server (which
 * checks it is a JPEG, PNG or WebP of up to 2 MB, with the token of the admin
 * login), its address goes into the field and is saved with the settings.
 */
function BackgroundUpload({ onDone }: { onDone: (url: string) => void }) {
  const t = useT()
  const token = useStore((st) => st.chatToken)
  const notify = useStore((st) => st.notify)
  const [busy, setBusy] = useState(false)
  const upload = async (f: File) => {
    setBusy(true)
    try {
      // (relative to the page: the server may sit behind a path)
      const res = await fetch(location.pathname.replace(/[^/]*$/, '') + 'background/upload', { method: 'POST', headers: { 'x-magicdab-token': token ?? '', 'content-type': f.type || 'application/octet-stream' }, body: f })
      if (res.ok) onDone(((await res.json()) as { url: string }).url)
      else {
        const why = res.status === 413 ? t('bg_upload_big') : res.status === 400 ? t('bg_upload_type') : `HTTP ${res.status}`
        notify('error', t('bg_upload_failed').replace('{e}', why))
      }
    } catch (e) {
      notify('error', t('bg_upload_failed').replace('{e}', String(e)))
    }
    setBusy(false)
  }
  return (
    <label className={'btn' + (busy ? ' disabled' : '')}>
      {busy ? '…' : t('bg_upload')}
      <input
        type="file"
        accept="image/jpeg,image/png,image/webp"
        hidden
        disabled={busy}
        onChange={(e) => {
          const f = e.target.files?.[0]
          if (f) upload(f)
          e.target.value = ''
        }}
      />
    </label>
  )
}

/** Listing on the public map of MagicDAB servers. */
function ServerMap() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const status: ServerMapStatus | undefined = useStore((st) => st.serverMapStatus ?? st.settings?.serverMap)
  const [f, setF] = useField('serverMap', { enabled: !!s?.serverMap?.enabled, url: s?.serverMap?.url ?? '', publicUrl: s?.serverMap?.publicUrl ?? '' })
  const tun = s?.tunnel
  const tunnelUrl = tun?.enabled && tun.subdomain && tun.host ? `https://${tun.subdomain}.${tun.host}` : ''
  return (
    <Card title={t('map_title')} note={t('map_help')}>
      <Row label={t('map_enable')}>
        <Toggle checked={f.enabled} onChange={(v) => setF({ ...f, enabled: v })} />
      </Row>
      <Row label={t('map_public_url')} hint={t('map_public_url_help')}>
        <input type="text" value={f.publicUrl} placeholder={tunnelUrl || 'https://…'} onChange={(e) => setF({ ...f, publicUrl: e.target.value })} />
      </Row>
      <Row label={t('map_url')}>
        <input type="text" value={f.url} placeholder={status?.defaultMap || 'https://dab.dxserver.eu'} onChange={(e) => setF({ ...f, url: e.target.value })} />
      </Row>
      <Row label={t('map_status')} wide>
        <MapStatus s={status} />
      </Row>
    </Card>
  )
}

/** Listed or not and why, the last error, the last successful heartbeat. */
function MapStatus({ s }: { s?: ServerMapStatus }) {
  const t = useT()
  // (registered, but the map has not verified the address yet)
  const state = s?.state === 'listed' && s.visible === false ? 'unverified' : s?.state || 'off'
  const time = (unix: number) => new Date(unix * 1000).toLocaleString()
  const head =
    state === 'unavailable' && s?.problem
      ? `${t('map_state_unavailable')}: ${t('map_problem_' + s.problem)}`
      : state === 'error' && s?.errorKind
        ? t('map_err_' + s.errorKind)
        : t('map_state_' + state)
  const on = s && state !== 'off'
  return (
    <div className="map-status">
      <span className={'map-state ' + state}>{head}</span>
      {(state === 'listed' || state === 'unverified') && s?.url && <span>{t('map_listed_as').replace('{u}', s.url)}</span>}
      {on && s.notice && (
        <span className="hint">
          {t('map_notice')}: {s.notice}
        </span>
      )}
      {on && s.error && (
        <span className="hint">
          {t('map_last_error')}: {s.error}
          {s.errorAt ? ` (${time(s.errorAt)})` : ''}
        </span>
      )}
      {on && s.retryAt && (
        <span className="hint">
          {t('map_next_try')}: {time(s.retryAt)}
        </span>
      )}
      {on && s.lastOk && (
        <span className="hint">
          {t('map_last_ok')}: {time(s.lastOk)}
        </span>
      )}
      {on && s.stationsAt && (
        <span className="hint">
          {t('map_stations_at')}: {time(s.stationsAt)}
        </span>
      )}
      {(state === 'listed' || state === 'unverified') && s?.page && (
        <a href={s.page} target="_blank" rel="noreferrer">
          {s.page}
        </a>
      )}
    </div>
  )
}

// ------------------------------------------------------------------ receiver

function Receiver() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const devices: string[] = useStore((st) => st.settings?.rtlsdrDevices ?? EMPTY)
  const state = useStore((st) => st.state)
  const [dev, setDev] = useField<any>('device', s?.device ?? { type: 'none' })
  const [ppm, setPpm] = useField('ppm', s?.ppm ?? 0)
  const [idle, setIdle] = useField('idleScan', s?.idleScan ?? { enabled: false, idleMinutes: 5, intervalMinutes: 0 })
  const [maxServices, setMaxServices] = useField('maxServices', s?.maxServices ?? 8)
  const [etiDelay, setEtiDelay] = useField('etiDelayMs', s?.etiDelayMs ?? 1000)
  const [bgPad, setBgPad] = useField('backgroundPad', s?.backgroundPad ?? true)
  const [spi, setSpi] = useField('spi', s?.spi ?? true)
  const [opus, setOpus] = useField('opusBitrate', s?.opusBitrate ?? 192000)
  const [streams, setStreams] = useField('streams', s?.streams ?? { enabled: false, max: 2, bitrate: 128 })
  const [lockCh, setLockCh] = useField<string | null>('lockChannel', s?.lockChannel ?? null)
  const [lockSvc, setLockSvc] = useField<string | null>('lockService', s?.lockService ?? null)
  const channels = useStore((st) => st.hello?.channels ?? EMPTY)
  const ensemble = useStore((st) => st.ensemble)
  if (!s) return null
  const d = state?.device
  // the stations of the multiplex received (and the one kept, if not on it)
  // (by the SId as the server writes it: hex, no leading zeros)
  const stations = (ensemble.services ?? []).filter((x) => !x.isData).map((x) => ({ sid: x.sid.toString(16).toUpperCase(), label: x.label }))
  if (lockSvc && !stations.some((x) => x.sid === lockSvc)) stations.push({ sid: lockSvc, label: lockSvc })
  return (
    <>
      {/* (a server's operator keeps it on a channel for the listeners; a phone has one listener) */}
      {!androidApp && (
      <Card title={t('set_card_lock')} note={t('set_lock_note')}>
        <Row label={t('set_lock_channel')}>
          <select value={lockCh ?? ''} onChange={(e) => setLockCh(e.target.value || null)}>
            <option value="">{t('set_lock_none')}</option>
            {channels.map((c: any) => (
              <option key={c.name} value={c.name}>
                {c.name}
              </option>
            ))}
          </select>
        </Row>
        <Row label={t('set_lock_service')}>
          <select value={lockSvc ?? ''} onChange={(e) => setLockSvc(e.target.value || null)}>
            <option value="">{t('set_lock_none')}</option>
            {stations.map((x) => (
              <option key={x.sid} value={x.sid}>
                {x.label} ({x.sid})
              </option>
            ))}
          </select>
        </Row>
      </Card>
      )}
      <Card title={t('set_card_input')}>
        {!androidApp && (
        <Row label={t('device')}>
          <select
            value={dev.type}
            onChange={(e) => {
              const type = e.target.value
              // (back to the saved input: as it was)
              if (type === s.device?.type) setDev(s.device)
              else if (type === 'rtlSdr') setDev({ type, index: 0, ppm: 0, bias_tee: false })
              else if (type === 'rtlTcp') setDev({ type, host: '127.0.0.1', port: 1234 })
              else if (type === 'rawFile') setDev({ type, path: '', looping: true })
              else if (type === 'hackrf') setDev({ type, serial: '', lna: 16, vga: 30 })
              else setDev({ type })
            }}
          >
            <option value="rtlSdr">RTL-SDR</option>
            <option value="rtlTcp">rtl_tcp</option>
            <option value="dtr100">DTR-100 (USB DAB)</option>
            <option value="hackrf">HackRF One</option>
            <option value="rawFile">{t('set_raw_file')}</option>
            <option value="none">{t('no_tuner')}</option>
          </select>
        </Row>
        )}
        {dev.type === 'rtlSdr' && (
          <>
            <Row label={t('rtl_index')}>
              <select value={dev.index ?? 0} onChange={(e) => setDev({ ...dev, index: parseInt(e.target.value) })}>
                {(devices.length ? devices : ['0']).map((x, i) => (
                  <option key={i} value={i}>
                    {x}
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Bias-T" hint={t('set_bias_hint')}>
              <Toggle checked={!!dev.bias_tee} onChange={(v) => setDev({ ...dev, bias_tee: v })} />
            </Row>
          </>
        )}
        {dev.type === 'dtr100' && <div className="hint">{t('set_dtr100_hint')}</div>}
        {dev.type === 'hackrf' && (
          <>
            <Row label={t('set_hackrf_serial')} hint={t('set_hackrf_serial_hint')}>
              <input type="text" value={dev.serial ?? ''} onChange={(e) => setDev({ ...dev, serial: e.target.value.trim() })} />
            </Row>
            <Row label="LNA" hint={t('set_hackrf_hint')}>
              <div className="set-inline">
                <select value={dev.lna ?? 16} onChange={(e) => setDev({ ...dev, lna: parseInt(e.target.value) })}>
                  {[0, 8, 16, 24, 32, 40].map((g) => (
                    <option key={g} value={g}>
                      {g} dB
                    </option>
                  ))}
                </select>
                VGA
                <select value={dev.vga ?? 30} onChange={(e) => setDev({ ...dev, vga: parseInt(e.target.value) })}>
                  {Array.from({ length: 32 }, (_, i) => i * 2).map((g) => (
                    <option key={g} value={g}>
                      {g} dB
                    </option>
                  ))}
                </select>
              </div>
            </Row>
          </>
        )}
        {dev.type === 'rtlTcp' && (
          <Row label={t('host_port')} hint={t('set_rtltcp_hint')}>
            <div className="set-inline">
              <input type="text" value={dev.host} onChange={(e) => setDev({ ...dev, host: e.target.value.trim() })} />
              <input type="number" className="num" min={1} max={65535} value={dev.port} onChange={(e) => setDev({ ...dev, port: parseInt(e.target.value) || 0 })} />
            </div>
          </Row>
        )}
        {dev.type === 'rawFile' && (
          <>
            <Row label={t('file_path')}>
              <input type="text" value={dev.path} placeholder="/data/recordings/…uff" onChange={(e) => setDev({ ...dev, path: e.target.value })} />
            </Row>
            <Row label={t('looping')}>
              <Toggle checked={!!dev.looping} onChange={(v) => setDev({ ...dev, looping: v })} />
            </Row>
          </>
        )}
        {d && (
          <Row label={t('set_device_state')}>
            <div className="set-status">
              <span className={d.error ? 'bad' : ''}>
                {d.name || ''}
                {d.error ? `: ${d.error}` : ''}
              </span>
              {d.kind === 'rtltcp' && (
                <span className="hint">
                  {t('stream_rate')}: {d.streamRate ? `${d.streamRate.toFixed(0)} kS/s` : ''}, {t('stream_stalls')}: {d.stalls ?? 0}
                  {d.lastStallMs ? ` (${t('last')}: ${(d.lastStallMs / 1000).toFixed(1)} s)` : ''}
                </span>
              )}
            </div>
          </Row>
        )}
        <Row label={t('ppm')} hint={t('set_ppm_hint')}>
          <input type="number" className="num" min={-200} max={200} value={ppm} onChange={(e) => setPpm(parseInt(e.target.value) || 0)} />
        </Row>
      </Card>
      <Gain />
      {!androidApp && <ChannelMaps />}
      <Card title={t('set_card_scan')}>
        <Row label={t('idle_scan')} hint={t('set_idle_hint')}>
          <Toggle checked={!!idle.enabled} onChange={(v) => setIdle({ ...idle, enabled: v })} />
        </Row>
        <Row label={t('idle_minutes')}>
          <input type="number" className="num" min={1} disabled={!idle.enabled} value={idle.idleMinutes} onChange={(e) => setIdle({ ...idle, idleMinutes: Math.max(1, parseInt(e.target.value) || 1) })} />
        </Row>
        <Row label={t('interval_minutes')}>
          <input type="number" className="num" min={0} disabled={!idle.enabled} value={idle.intervalMinutes} onChange={(e) => setIdle({ ...idle, intervalMinutes: Math.max(0, parseInt(e.target.value) || 0) })} />
        </Row>
      </Card>
      <Card title={t('set_card_decoding')}>
        <Row label={t('set_eti_delay')} hint={t('set_eti_delay_hint')}>
          <input type="number" className="num" min={0} max={10000} step={100} value={etiDelay} onChange={(e) => setEtiDelay(Math.max(0, Math.min(10000, parseInt(e.target.value) || 0)))} />
        </Row>
        <Row label={t('max_services')} hint={t('set_max_services_hint')}>
          <input type="number" className="num" min={1} max={32} value={maxServices} onChange={(e) => setMaxServices(Math.max(1, Math.min(32, parseInt(e.target.value) || 1)))} />
        </Row>
        <Row label={t('background_pad')}>
          <Toggle checked={bgPad} onChange={setBgPad} />
        </Row>
        <Row label={t('spi')}>
          <Toggle checked={spi} onChange={setSpi} />
        </Row>
        <Row label={t('opus_bitrate')} hint={t('set_opus_hint')}>
          <select value={opus} onChange={(e) => setOpus(parseInt(e.target.value))}>
            {[64000, 96000, 128000, 160000, 192000, 256000].map((b) => (
              <option key={b} value={b}>
                {b / 1000} kbit/s
              </option>
            ))}
          </select>
        </Row>
      </Card>
      {!androidApp && (
      <Card title={t('set_card_streams')}>
        <Row label={t('streams_on')} hint={t('set_streams_hint').replace('{url}', `${location.origin}${location.pathname.replace(/[^/]*$/, '')}stream/<SId>`)}>
          <Toggle checked={!!streams.enabled} onChange={(v) => setStreams({ ...streams, enabled: v })} />
        </Row>
        <Row label={t('streams_max')} hint={t('set_streams_max_hint')}>
          <input type="number" className="num" min={1} max={16} disabled={!streams.enabled} value={streams.max} onChange={(e) => setStreams({ ...streams, max: Math.max(1, Math.min(16, parseInt(e.target.value) || 1)) })} />
        </Row>
        <Row label={t('streams_bitrate')}>
          <select value={streams.bitrate} disabled={!streams.enabled} onChange={(e) => setStreams({ ...streams, bitrate: parseInt(e.target.value) })}>
            {[64, 96, 128, 160, 192, 256].map((b) => (
              <option key={b} value={b}>
                {b} kbit/s
              </option>
            ))}
          </select>
        </Row>
      </Card>
      )}
    </>
  )
}

/** Gain works at once: it is listened to while being set. */
function Gain() {
  const t = useT()
  const saved = useStore((st) => st.settings?.settings?.gain)
  const state = useStore((st) => st.state)
  const [gain, setGain] = useState<any>(saved)
  useEffect(() => {
    setGain(saved)
  }, [saved])
  if (!gain) return null
  const gains = state?.device.gains ?? []
  const apply = (g: any) => {
    setGain(g)
    saveSettings({ gain: g })
  }
  return (
    <Card title={t('set_card_gain')} note={t('set_gain_live')}>
      <Row label={t('gain_mode')}>
        <Choice
          value={gain.mode}
          options={[
            ['software', t('gain_software')],
            ['hardware', t('gain_hardware')],
            ['manual', t('gain_manual')],
          ]}
          onChange={(mode) => apply(mode === 'manual' ? { mode, index: state?.device.gainIndex ?? Math.floor(gains.length / 2) } : { mode })}
        />
      </Row>
      {gain.mode === 'manual' && gains.length > 0 && (
        <Row label={t('set_gain_value')}>
          <div className="set-inline">
            <input type="range" min={0} max={gains.length - 1} value={gain.index} onChange={(e) => apply({ mode: 'manual', index: parseInt(e.target.value) })} />
            <span className="set-value">{gains[gain.index]?.toFixed(1)} dB</span>
          </div>
        </Row>
      )}
    </Card>
  )
}

/** Channels fed by ETI / EDI streams (e.g. ODR-DabMux) or IQ recordings instead of the tuner. */
function ChannelMaps() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const channels = useStore((st) => st.hello?.channels ?? EMPTY)
  const state = useStore((st) => st.state)
  const [maps, setMaps] = useField<{ channel: string; url: string }[]>('channelMaps', s?.channelMaps ?? EMPTY)
  const [own, setOwn] = useField('listenerMultiplex', s?.listenerMultiplex ?? true)
  const [limit, setLimit] = useField('listenerMultiplexLimit', s?.listenerMultiplexLimit ?? 4)
  const edit = (i: number, m: Partial<{ channel: string; url: string }>) => setMaps(maps.map((x, j) => (j === i ? { ...x, ...m } : x)))
  return (
    <Card title={t('set_card_maps')} note={t('channel_maps_help')}>
      <div className="set-row wide">
        <div className="maps">
          {maps.map((m, i) => (
            <div key={i} className="map-row">
              <select value={m.channel} onChange={(e) => edit(i, { channel: e.target.value })}>
                {channels.map((c) => (
                  <option key={c.name} value={c.name}>
                    {c.name}
                  </option>
                ))}
              </select>
              <input type="text" className={validMap(m.url) ? '' : 'invalid'} value={m.url} placeholder="edi+tcp://host:13000" onChange={(e) => edit(i, { url: e.target.value })} />
              <button className="icon-btn" onClick={() => setMaps(maps.filter((_, j) => j !== i))} aria-label={t('remove')} title={t('remove')}>
                ✕
              </button>
            </div>
          ))}
          {!maps.length && <div className="hint">{t('set_no_maps')}</div>}
          <div className="set-inline">
            <button className="btn" onClick={() => setMaps([...maps, { channel: channels.find((c) => !maps.some((m) => m.channel === c.name))?.name ?? '5A', url: '' }])}>
              + {t('add_map')}
            </button>
            {state?.input === 'eti' && (
              <span className="hint">
                {state.channel}: {state.etiConnected ? t('eti_connected') : t('eti_connecting')}
              </span>
            )}
          </div>
        </div>
      </div>
      <Row label={t('set_listener_mux')} hint={t('set_listener_mux_hint')}>
        <Toggle checked={own} onChange={setOwn} label={t('set_listener_mux')} />
      </Row>
      <Row label={t('set_listener_mux_limit')} hint={t('set_listener_mux_limit_hint')}>
        <input
          type="number"
          className="num"
          min={0}
          max={16}
          disabled={!own}
          value={limit}
          onChange={(e) => setLimit(Math.max(0, Math.min(16, parseInt(e.target.value) || 0)))}
        />
      </Row>
    </Card>
  )
}

// ------------------------------------------------------------------ FM

function FmSettings() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const fmPorts: string[] = useStore((st) => st.settings?.fmPorts ?? EMPTY)
  const fmAudios: [string, string][] = useStore((st) => st.settings?.fmAudioDevices ?? EMPTY)
  const [enabled, setEnabled] = useField('fmEnabled', s?.fm?.enabled ?? true)
  const [port, setPort] = useField('fmPort', s?.fm?.port ?? '')
  const [audio, setAudio] = useField('fmAudio', s?.fm?.audio ?? '')
  const [antennas, setAntennas] = useField('fmAntennas', s?.fm?.antennas ?? 1)
  const [linking, setLinking] = useField('linkingEnabled', s?.linking?.enabled ?? true)
  const [matchLevel, setMatchLevel] = useField('linkingMatchLevel', s?.linking?.matchLevel ?? true)
  if (!s) return null
  return (
    <>
      <Card title={t('set_card_fm')} note={t('set_fm_note')}>
        <Row label={t('set_fm_enabled')}>
          <Toggle checked={enabled} onChange={setEnabled} />
        </Row>
        <Row label={t('set_fm_port')}>
          <select value={port} disabled={!enabled} onChange={(e) => setPort(e.target.value)}>
            <option value="">{t('set_auto')}</option>
            {[...new Set([...fmPorts, ...(port ? [port] : [])])].map((p) => (
              <option key={p} value={p}>
                {p.replace(/^\/dev\/(hostdev\/)?/, '')}
              </option>
            ))}
          </select>
        </Row>
        <Row label={t('set_fm_audio')}>
          <select value={audio} disabled={!enabled} onChange={(e) => setAudio(e.target.value)}>
            <option value="">{t('set_auto_tef')}</option>
            {fmAudios.map(([dev, name]) => (
              <option key={dev} value={dev}>
                {name} ({dev})
              </option>
            ))}
            {audio && !fmAudios.some(([x]) => x === audio) && <option value={audio}>{audio}</option>}
          </select>
        </Row>
        <Row label={t('fm_antennas')}>
          <input type="number" className="num" min={1} max={4} disabled={!enabled} value={antennas} onChange={(e) => setAntennas(Math.max(1, Math.min(4, parseInt(e.target.value) || 1)))} />
        </Row>
      </Card>
      <Card title={t('set_card_linking')} badge={t('set_experimental')} note={t('set_linking_experimental')}>
        <Row label={t('set_linking_enabled')} hint={enabled ? undefined : t('set_linking_needs_fm')}>
          <Toggle checked={linking && enabled} disabled={!enabled} onChange={setLinking} />
        </Row>
        <Row label={t('set_linking_match')} hint={t('set_linking_match_hint')}>
          <Toggle checked={matchLevel} disabled={!enabled || !linking} onChange={setMatchLevel} />
        </Row>
      </Card>
    </>
  )
}

// ------------------------------------------------------------------ TII

function TiiSettings({ go }: { go: (tab: Tab) => void }) {
  const t = useT()
  const admin = useStore((st) => st.admin)
  const s = useStore((st) => st.settings?.settings)
  const hello = useStore((st) => st.hello)
  const [mode, setMode] = useField('tiiMode', s?.tiiMode ?? 'sensitive')
  const [log, setLog] = useField('tiiLog', !!s?.tiiLog)
  const loc = s?.location ?? hello?.location
  return (
    <>
      <Card title={t('set_card_location')}>
        <Row label={loc ? locText(loc) : t('set_no_location')} hint={t('set_tii_location_hint')}>
          {admin && (
            <button className="btn" onClick={() => go('station')}>
              {t('set_change')}
            </button>
          )}
        </Row>
      </Card>
      <Card title={t('set_card_txdb')} note={t('set_txdb_hint')}>
        <Row label={t('transmitters')}>
          <div className="set-inline">
            <span className="set-value">
              {hello?.txdb.count ?? 0}
              {hello?.txdb.updated ? ` (${new Date(hello.txdb.updated * 1000).toLocaleDateString()})` : ''}
            </span>
            {admin && (
              <label className="btn">
                {t('import_db')}
                <input
                  type="file"
                  accept=".csv,text/csv,text/plain"
                  hidden
                  onChange={async (e) => {
                    const f = e.target.files?.[0]
                    if (f) importTxDb(await f.text())
                    e.target.value = ''
                  }}
                />
              </label>
            )}
          </div>
        </Row>
      </Card>
      {admin && s && (
        <Card title={t('set_card_tii')}>
          <Row label={t('tii_mode')} hint={t('set_tii_mode_hint')}>
            <Choice
              value={mode}
              options={[
                ['sensitive', t('tii_sensitive')],
                ['reliable', t('tii_reliable')],
              ]}
              onChange={setMode}
            />
          </Row>
          <Row label={t('tii_log')}>
            <Toggle checked={log} onChange={setLog} />
          </Row>
        </Card>
      )}
    </>
  )
}

// ------------------------------------------------------------------ internet

function Internet() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const [internet, setInternet] = useField('internet', s?.internet ?? true)
  const [vis, setVis] = useField('radioVis', s?.radioVis ?? true)
  // the server never sends the webhook: only that there is one
  const hasHook = !!s?.hasDiscordWebhook
  const [hook, setHook, hookChanged] = useField<string | null>('discordWebhook', '')
  const [chat, setChat] = useField('chatEnabled', s?.chatEnabled ?? true)
  const [noIndex, setNoIndex] = useField('noIndex', !!s?.noIndex)
  if (!s) return null
  return (
    <>
      <Card title={t('set_card_online')}>
        <Row label={t('internet')} hint={t('set_internet_hint')}>
          <Toggle checked={internet} onChange={setInternet} />
        </Row>
        <Row label={t('radio_vis')}>
          <Toggle checked={vis && internet} disabled={!internet} onChange={setVis} />
        </Row>
      </Card>
      {!androidApp && (
      <Card title={t('set_card_notify')}>
        <Row
          label={t('discord_webhook')}
          hint={[t('set_discord_hint'), hook === null ? t('secret_removed') : hasHook ? t('secret_kept') : ''].filter(Boolean).join(' ')}
          wide
        >
          <div className="set-inline">
            <input
              type="password"
              autoComplete="new-password"
              className={fieldOk('discordWebhook', hook) ? '' : 'invalid'}
              placeholder={hasHook && hook !== null ? '••••••••' : 'https://discord.com/api/webhooks/…'}
              value={hook ?? ''}
              onChange={(e) => setHook(e.target.value)}
            />
            {hasHook && hook !== null && (
              <button className="btn" onClick={() => setHook(null)}>
                {t('remove')}
              </button>
            )}
            <button className="btn" disabled={hookChanged || !hasHook} title={hookChanged ? t('set_save_first') : undefined} onClick={() => send({ type: 'testDiscord' })}>
              Test
            </button>
          </div>
        </Row>
      </Card>
      )}
      {!androidApp && (
      <Card title={t('set_card_listeners')}>
        <Row label={t('set_chat_enabled')}>
          <Toggle checked={chat} onChange={setChat} />
        </Row>
        <Row label={t('no_index')}>
          <Toggle checked={noIndex} onChange={setNoIndex} />
        </Row>
      </Card>
      )}
      <Updates />
    </>
  )
}

/** Updates of the program (manifest address, check, install; the Android app: check and install, Android asks). */
function Updates() {
  const t = useT()
  const u = useStore((st) => st.update)
  const [url, setUrl, urlChanged] = useField('updateUrl', u?.customUrl ?? '')
  const [auto, setAuto] = useField('updateAuto', u?.auto ?? false)
  if (!u) return null
  return (
    <Card title={t('upd_title')}>
      <Row label={t('upd_version')}>
        <span className="set-value">
          {u.current} {u.platform ? `(${u.platform})` : ''}
        </span>
      </Row>
      {u.container ? (
        <Row label={t('upd_container')} />
      ) : (
        <>
          {/* (the app: the manifest of its build, and Android asks before each install) */}
          {androidApp ? (
            <p className="set-intro hint">{t('upd_android')}</p>
          ) : (
            <>
              <Row label={t('upd_url')} hint={u.defaultUrl ? t('set_upd_default').replace('{u}', u.defaultUrl) : undefined}>
                <input type="text" className={fieldOk('updateUrl', url) ? '' : 'invalid'} placeholder={u.defaultUrl || 'https://…/latest.json'} value={url} onChange={(e) => setUrl(e.target.value)} />
              </Row>
              <Row label={t('upd_auto')}>
                <Toggle checked={auto} onChange={setAuto} />
              </Row>
            </>
          )}
          <Row
            label={u.busy ? t('upd_busy') : u.available ? t('upd_available').replace('{v}', u.available.version) : u.url && !u.error ? t('upd_latest') : ''}
            hint={
              u.error ? (
                <span className="bad">{u.error}</span>
              ) : u.available?.notes ? (
                <span className="set-notes">{u.available.notes}</span>
              ) : undefined
            }
          >
            <div className="set-inline">
              <button className="btn" disabled={u.busy || !u.url || urlChanged} title={urlChanged ? t('set_save_first') : undefined} onClick={() => send({ type: 'updateCheck' })}>
                {t('upd_check')}
              </button>
              {u.available && (
                <button className="btn primary" disabled={u.busy} onClick={() => installApk(u.available!) || send({ type: 'updateInstall' })}>
                  {t('upd_install')}
                </button>
              )}
            </div>
          </Row>
        </>
      )}
    </Card>
  )
}

// ------------------------------------------------------------------ tunnel

/** Public tunnel through an frp server (as in fm-dx-webserver). */
function Tunnel() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const status = useStore((st) => st.tunnelStatus ?? st.settings?.tunnel)
  const [f, setF, changed] = useField<any>('tunnel', { host: 'dxserver.eu', port: 7000, ...(s?.tunnel ?? {}) })
  if (!s) return null
  const url = s.tunnel?.enabled && s.tunnel.subdomain && s.tunnel.host ? `https://${s.tunnel.subdomain}.${s.tunnel.host}` : ''
  const state = status?.state ?? 'off'
  return (
    <>
      <Card title={t('set_card_tunnel')} note={t('set_tunnel_note')}>
        <Row label={t('tunnel_enable')}>
          <Toggle checked={!!f.enabled} onChange={(v) => setF({ ...f, enabled: v })} />
        </Row>
        <Row label={t('set_device_state')}>
          <div className="set-status">
            <span className={'tunnel-state ' + state}>
              {t('tunnel_state_' + state)}
              {status?.message ? `: ${status.message}` : ''}
            </span>
            {url && !changed && (
              <a href={url} target="_blank" rel="noreferrer">
                {url}
              </a>
            )}
          </div>
        </Row>
        <Row label={t('tunnel_host')}>
          <div className="set-inline">
            <input type="text" value={f.host} placeholder="dxserver.eu" onChange={(e) => setF({ ...f, host: e.target.value.trim() })} />
            <input type="number" className="num" value={f.port} onChange={(e) => setF({ ...f, port: parseInt(e.target.value) || 0 })} />
          </div>
        </Row>
        <Row label={t('tunnel_user')}>
          <input type="text" value={f.username ?? ''} onChange={(e) => setF({ ...f, username: e.target.value.trim() })} />
        </Row>
        <Row label={t('tunnel_subdomain')} hint={f.subdomain && f.host ? `https://${f.subdomain}.${f.host}` : undefined}>
          <input type="text" value={f.subdomain ?? ''} onChange={(e) => setF({ ...f, subdomain: e.target.value.trim().toLowerCase() })} />
        </Row>
        {/* the server never sends the token: only that there is one (empty keeps it) */}
        <Row label={t('tunnel_token')} hint={s.tunnel?.hasToken ? t('secret_kept') : undefined}>
          <input
            type="password"
            value={f.token ?? ''}
            placeholder={s.tunnel?.hasToken ? '••••••••' : ''}
            autoComplete="new-password"
            onChange={(e) => setF({ ...f, token: e.target.value.trim() })}
          />
        </Row>
        <Row label={t('tunnel_kcp')}>
          <Toggle checked={!!f.lowLatency} onChange={(v) => setF({ ...f, lowLatency: v })} />
        </Row>
      </Card>
    </>
  )
}

// ------------------------------------------------------------------ the view

export default function SettingsView() {
  const t = useT()
  const admin = useStore((s) => s.admin)
  const [tab, setTab] = useState<Tab>('general')
  const draft = useDraft((d) => d.values)
  const changedTabs = new Set(Object.keys(draft).map((k) => TAB_OF[k]))
  // (no FM tuner in the Android app)
  const fmAvailable = useStore((s) => s.settings?.fmAvailable !== false)
  // (the server's tabs, TII among them, only for the logged in admin)
  // (the Android app: the stick of the phone, nothing made public)
  const tabs: Tab[] = admin
    ? ['general', ...SERVER_TABS.filter((x) => (x !== 'fm' || fmAvailable) && !(androidApp && PUBLIC_TABS.includes(x)))]
    : ['general']
  // (logged out: back to what is left)
  const current = tabs.includes(tab) ? tab : 'general'
  // (another tab starts at its top; on a phone the row of tabs shows the chosen one)
  const body = useRef<HTMLDivElement>(null)
  const nav = useRef<HTMLElement>(null)
  useEffect(() => {
    // (a block: scrolling returns a promise in new browsers, not a clean-up)
    body.current?.scrollTo(0, 0)
    const n = nav.current
    const el = n?.querySelector<HTMLElement>('.settings-nav-item.active')
    if (n && el && n.scrollWidth > n.clientWidth) n.scrollLeft = el.offsetLeft - (n.clientWidth - el.offsetWidth) / 2
  }, [current])
  const label = (x: Tab) =>
    t(x === 'station' ? (androidApp ? 'set_tab_station_app' : 'set_tab_station') : x === 'fm' ? 'set_tab_fm' : x === 'internet' ? 'set_tab_internet' : x)
  const item = (x: Tab) => (
    <button key={x} role="tab" aria-selected={current === x} className={'settings-nav-item' + (current === x ? ' active' : '')} onClick={() => setTab(x)}>
      {label(x)}
      {changedTabs.has(x) && <span className="set-dot" title={t('set_changed')} />}
    </button>
  )
  return (
    <>
      <ViewHeader />
      <div className="settings">
        <nav className="settings-nav" role="tablist" ref={nav}>
          <div className="settings-nav-group">{t('set_group_browser')}</div>
          {item('general')}
          {admin && <div className="settings-nav-group">{t('set_group_server')}</div>}
          {tabs.filter((x) => x !== 'general').map(item)}
          {admin && !androidApp && (
            <button className="settings-nav-item logout" onClick={logout}>
              {t('logout')}
            </button>
          )}
        </nav>
        <div className="settings-body" ref={body}>
          <div className="settings-page" key={current}>
            {current === 'general' && <General />}
            {current === 'station' && <Station />}
            {current === 'receiver' && <Receiver />}
            {current === 'fm' && <FmSettings />}
            {current === 'tii' && <TiiSettings go={setTab} />}
            {current === 'internet' && <Internet />}
            {current === 'access' && <AccessSettings />}
            {current === 'tunnel' && <Tunnel />}
          </div>
          <SaveBar where={(key) => label(TAB_OF[key] ?? 'general')} />
        </div>
      </div>
    </>
  )
}
