import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { ArrowLeftRight, ChevronDown, ChevronRight, Lock, Search, X } from 'lucide-react'
import { useStore, save, useChannelAllowed } from '../store'
import { useT } from '../i18n'
import { Logo, FavStar, SignalBars, scrollIntoList } from '../components/common'
import { selectService, tuningLocked } from '../actions'
import { ageShort, fullSid, hex } from '../util'

export type StationMode = 'mux' | 'all' | 'fav'

/** One multiplex carrying a station (from the service list). */
interface MuxRef {
  channel: string
  eid: number
  label: string
  snr: number
  lastSeen: number
}

interface Station {
  key: string
  sid: number
  ecc: number | null
  label: string
  muxes: MuxRef[]
}

const FRESH = 6 * 3600

/** Stable fallback for store selectors (a new [] each time loops with zustand 5). */
const EMPTY: string[] = []

/** Search text and scroll positions survive switching views (the browser unmounts). */
const memo: { query: Record<string, string>; scroll: Record<string, number>; collapsed: Record<string, Record<string, boolean>> } = {
  query: {},
  scroll: {},
  collapsed: {},
}

function loadFlag(key: string, def: boolean) {
  try {
    const v = localStorage.getItem('magicdab.' + key)
    return v === null ? def : (JSON.parse(v) as boolean)
  } catch {
    return def
  }
}

function snrLevel(snr: number) {
  return snr >= 10 ? 3 : snr >= 6 ? 2 : snr > 0 ? 1 : 0
}

/** Best multiplex to tune for a station: the current one, else the best fresh signal, else the newest. */
function bestMux(muxes: MuxRef[], current: string | null | undefined) {
  const now = Date.now() / 1000
  return (
    muxes.find((m) => m.channel === current) ??
    [...muxes].sort((a, b) => {
      const fa = now - a.lastSeen < FRESH ? 1 : 0
      const fb = now - b.lastSeen < FRESH ? 1 : 0
      return fb - fa || (fa ? b.snr - a.snr : b.lastSeen - a.lastSeen)
    })[0]
  )
}

function MuxChips({ muxes, current }: { muxes: MuxRef[]; current: string | null | undefined }) {
  const lang = useStore((s) => s.lang)
  const now = Date.now() / 1000
  return (
    <span className="mux-chips">
      {muxes.map((m) => {
        const fresh = now - m.lastSeen < FRESH
        return (
          <span
            key={m.channel + m.eid}
            className={'mux-chip' + (m.channel === current ? ' current' : '') + (fresh ? '' : ' stale')}
            title={`${m.label} · ${fresh && m.snr ? m.snr.toFixed(1) + ' dB · ' : ''}${ageShort(m.lastSeen, lang)}`}
          >
            {fresh && m.snr > 0 && <SignalBars level={snrLevel(m.snr)} synced />}
            {m.channel}
          </span>
        )
      })}
    </span>
  )
}

/** Station browser: current multiplex, all stations found by scans, favourites. */
export function StationBrowser({ full, panel }: { full?: boolean; panel?: boolean }) {
  const t = useT()
  const ensemble = useStore((s) => s.ensemble)
  const serviceList = useStore((s) => s.serviceList)
  const selected = useStore((s) => s.selected)
  const favorites = useStore((s) => s.favorites)
  const dlsAll = useStore((s) => s.dlsAll)
  const admin = useStore((s) => s.admin)
  const locked = useStore((s) => tuningLocked(s) || !!(s.state?.fixedChannel ?? s.hello?.fixedChannel))
  // multiplexes this listener may tune to (and the current one)
  const allowed = useChannelAllowed()
  // mapped channels a listener plays on a receiver of its own (the lock is for the main one)
  const ownMuxes = useStore((s) => (s.hello?.listenerMultiplex && !s.admin ? s.state?.mappedChannels ?? EMPTY : EMPTY))
  // on its own multiplex: where the main receiver is (phones have no channel row)
  const own = useStore((s) => (s.state?.slot ? s.state : null))
  // the desktop side panel sits next to the list of the current multiplex
  const storeKey = panel ? 'stationModePanel' : 'stationMode'
  const [mode, setModeState] = useState<StationMode>(() => {
    try {
      const m = JSON.parse(localStorage.getItem('magicdab.' + storeKey) ?? 'null') as StationMode | null
      return m && !(panel && m === 'mux') ? m : panel ? 'all' : 'mux'
    } catch {
      return panel ? 'all' : 'mux'
    }
  })
  const where = panel ? 'panel' : 'full'
  const [grouped, setGroupedState] = useState(() => loadFlag('grouped.' + where, false))
  const [query, setQueryState] = useState(memo.query[where] ?? '')
  const [collapsed, setCollapsedState] = useState<Record<string, boolean>>(memo.collapsed[where] ?? {})
  const setMode = (m: StationMode) => {
    setModeState(m)
    save(storeKey, m)
  }
  const setGrouped = (g: boolean) => {
    setGroupedState(g)
    save('grouped.' + where, g)
  }
  const setQuery = (q: string) => {
    setQueryState(q)
    memo.query[where] = q
  }
  const setCollapsed = (c: Record<string, boolean>) => {
    setCollapsedState(c)
    memo.collapsed[where] = c
  }
  // keep the place in the list when coming back to it
  const listRef = useRef<HTMLUListElement>(null)
  const scrollKey = `${where}:${mode}:${grouped}`
  useLayoutEffect(() => {
    if (listRef.current) listRef.current.scrollTop = memo.scroll[scrollKey] ?? 0
  }, [scrollKey])
  const onScroll = (e: React.UIEvent<HTMLUListElement>) => {
    memo.scroll[scrollKey] = e.currentTarget.scrollTop
  }
  const current = ensemble.valid ? ensemble.channel : null
  const isPlaying = (sid: number, channel?: string | null) => selected?.sid === sid && (!channel || channel === current)
  const ownMux = (ch: string) => ownMuxes.includes(ch) && allowed(ch)
  const reachable = (ch: string) => ch === current || !locked || admin || ownMux(ch)
  // the station playing comes into sight when it changes (also from elsewhere)
  useEffect(() => {
    const id = requestAnimationFrame(() => {
      const row = listRef.current?.querySelector<HTMLElement>('.st-row.selected')
      if (row) scrollIntoList(row)
    })
    return () => cancelAnimationFrame(id)
  }, [selected?.sid, current, mode, grouped])

  const stations = useMemo(() => {
    const map = new Map<string, Station>()
    for (const e of serviceList) {
      if (!allowed(e.channel) && e.channel !== ensemble.channel) continue
      for (const s of e.services) {
        const ecc = s.ecc ?? e.ecc ?? null
        const key = `${ecc ?? ''}:${s.sid}`
        let st = map.get(key)
        if (!st) {
          st = { key, sid: s.sid, ecc, label: s.label, muxes: [] }
          map.set(key, st)
        }
        if (!st.muxes.some((m) => m.channel === e.channel && m.eid === e.eid)) {
          st.muxes.push({ channel: e.channel, eid: e.eid, label: e.label, snr: e.snr, lastSeen: e.lastSeen })
        }
      }
    }
    for (const st of map.values()) st.muxes.sort((a, b) => b.lastSeen - a.lastSeen || a.channel.localeCompare(b.channel))
    return [...map.values()].sort((a, b) => a.label.localeCompare(b.label))
  }, [serviceList, allowed, ensemble.channel])

  const q = query.trim().toLowerCase()
  const match = (label: string, sid: number) => !q || label.toLowerCase().includes(q) || hex(sid).toLowerCase().includes(q)

  const muxServices = (ensemble.services ?? []).filter((s) => s.isAudio && match(s.label, s.sid))
  const favStations = stations.filter((s) => favorites.includes(s.sid) && match(s.label, s.sid))
  // favourites only received live (not in the service list yet)
  const liveFavs = (ensemble.services ?? []).filter(
    (s) => s.isAudio && favorites.includes(s.sid) && !stations.some((x) => x.sid === s.sid) && match(s.label, s.sid),
  )

  // the first tap plays the station, another tap on the playing station moves
  // it to the next multiplex carrying it
  const play = (st: Station) => {
    if (isPlaying(st.sid) && st.muxes.length > 1 && st.muxes.some((m) => m.channel === current)) {
      // the next multiplex carrying it that this listener may tune to
      const i = st.muxes.findIndex((m) => m.channel === current)
      const next = [...st.muxes.slice(i + 1), ...st.muxes.slice(0, i)].find((m) => reachable(m.channel))
      if (!next) {
        useStore.getState().notify('warning', 'tuner_locked')
        return
      }
      selectService(st.sid, 0, next.channel)
      return
    }
    const m = bestMux(st.muxes.filter((x) => reachable(x.channel)), current) ?? bestMux(st.muxes, current)
    if (!m) return
    selectService(st.sid, 0, m.channel)
  }
  const stationOf = (sid: number, ecc: number | null | undefined) =>
    stations.find((x) => x.sid === sid && (x.ecc === null || ecc == null || x.ecc === ecc))

  const stationRow = (st: Station) => {
    const onCurrent = st.muxes.some((m) => m.channel === current)
    const blocked = locked && !admin && !st.muxes.some((m) => reachable(m.channel))
    return (
      <li
        key={st.key}
        className={'st-row' + (isPlaying(st.sid) && onCurrent ? ' selected' : '') + (blocked ? ' blocked' : '')}
        onClick={() => play(st)}
      >
        <Logo label={st.label} sid={st.sid} ecc={st.ecc} small />
        <div className="st-txt">
          <div className="st-name">{st.label}</div>
          <div className="st-sub">
            <MuxChips muxes={st.muxes} current={current} />
          </div>
        </div>
        {blocked && <Lock size={16} className="st-lock" />}
        <FavStar sid={st.sid} />
      </li>
    )
  }

  return (
    <div className={'stations' + (full ? ' full' : '')}>
      {full && own && (
        <div className="own-mux hint">
          {t('own_mux_note')
            .replace('{c}', own.channel ?? '')
            .replace('{m}', own.mainChannel ?? '–')}
        </div>
      )}
      <div className="st-tools">
        <div className="seg">
          {!panel && (
            <button className={mode === 'mux' ? 'on' : ''} onClick={() => setMode('mux')}>
              {t('this_mux')}
            </button>
          )}
          <button className={mode === 'all' ? 'on' : ''} onClick={() => setMode('all')}>
            {t('all_stations')} <span className="cnt">{stations.length}</span>
          </button>
          <button className={mode === 'fav' ? 'on' : ''} onClick={() => setMode('fav')}>
            {t('favorites')}
          </button>
        </div>
        <div className="st-search">
          <Search size={16} />
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('search_station')} />
          {query && (
            <button className="icon-btn" onClick={() => setQuery('')} aria-label="clear">
              <X size={16} />
            </button>
          )}
        </div>
        {mode === 'all' && (
          <label className="st-group">
            <input type="checkbox" checked={grouped} onChange={(e) => setGrouped(e.target.checked)} /> {t('group_by_mux')}
          </label>
        )}
      </div>

      {mode === 'mux' && (
        <ul className="st-list" ref={listRef} onScroll={onScroll}>
          {!ensemble.valid && <li className="st-empty">{t('no_ensemble')}</li>}
          {muxServices.map((s) => (
            <li
              key={s.sid}
              className={'st-row' + (isPlaying(s.sid) ? ' selected' : '')}
              onClick={() => {
                const st = stationOf(s.sid, s.ecc)
                if (st && isPlaying(s.sid)) play(st)
                else selectService(s.sid, 0)
              }}
            >
              <Logo label={s.label} sid={s.sid} small />
              <div className="st-txt">
                <div className="st-name">{s.label || s.sidHex}</div>
                <div className="st-sub">{dlsAll[s.sid] ? <span className="st-dls">{dlsAll[s.sid]}</span> : <span className="hint">{fullSid(s)}</span>}</div>
              </div>
              {(stationOf(s.sid, s.ecc)?.muxes.length ?? 0) > 1 && (
                <span className="st-more" title={t('tap_again_mux')}>
                  <ArrowLeftRight size={13} />
                  {(stationOf(s.sid, s.ecc)?.muxes.length ?? 1) - 1}
                </span>
              )}
              <FavStar sid={s.sid} />
            </li>
          ))}
        </ul>
      )}

      {mode === 'all' && !grouped && (
        <ul className="st-list" ref={listRef} onScroll={onScroll}>
          {stations.filter((s) => match(s.label, s.sid)).map(stationRow)}
          {!stations.length && <li className="st-empty">{t('no_stations')}</li>}
        </ul>
      )}

      {mode === 'all' && grouped && (
        <ul className="st-list" ref={listRef} onScroll={onScroll}>
          {serviceList.map((e) => {
            const key = e.channel + e.eid
            const open = !collapsed[key] || !!q
            const rows = e.services.filter((s) => match(s.label, s.sid))
            if (q && !rows.length) return null
            const isCur = e.channel === current && ensemble.eid === e.eid
            if (!allowed(e.channel) && !isCur) return null
            const now = Date.now() / 1000
            const fresh = now - e.lastSeen < FRESH
            return (
              <li key={key} className="st-group-item">
                <div className={'st-mux' + (isCur ? ' current' : '')} onClick={() => setCollapsed({ ...collapsed, [key]: open })}>
                  {open ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                  <span className="mux-chip current-able">{e.channel}</span>
                  <span className="st-mux-name">{e.label}</span>
                  {fresh && e.snr > 0 && (
                    <span className="mux-signal">
                      <SignalBars level={snrLevel(e.snr)} synced /> {e.snr.toFixed(1)} dB
                    </span>
                  )}
                  <span className="cnt">{e.services.length}</span>
                </div>
                {open && (
                  <ul className="st-list nested">
                    {rows.map((s) => {
                      const blocked = locked && !isCur && !admin && !ownMux(e.channel)
                      return (
                        <li
                          key={s.sid}
                          className={'st-row' + (isPlaying(s.sid, e.channel) && isCur ? ' selected' : '') + (blocked ? ' blocked' : '')}
                          onClick={() => selectService(s.sid, s.scids, e.channel)}
                        >
                          <Logo label={s.label} sid={s.sid} ecc={s.ecc ?? e.ecc} small />
                          <div className="st-txt">
                            <div className="st-name">{s.label}</div>
                            <div className="st-sub hint">
                              {s.codec} {s.bitrate ? `· ${s.bitrate} kbps` : ''}
                            </div>
                          </div>
                          {blocked && <Lock size={16} className="st-lock" />}
                          <FavStar sid={s.sid} />
                        </li>
                      )
                    })}
                  </ul>
                )}
              </li>
            )
          })}
        </ul>
      )}

      {mode === 'fav' && (
        <ul className="st-list" ref={listRef} onScroll={onScroll}>
          {liveFavs.map((s) => (
            <li key={'live' + s.sid} className={'st-row' + (isPlaying(s.sid) ? ' selected' : '')} onClick={() => selectService(s.sid, 0)}>
              <Logo label={s.label} sid={s.sid} small />
              <div className="st-txt">
                <div className="st-name">{s.label}</div>
                <div className="st-sub hint">{current}</div>
              </div>
              <FavStar sid={s.sid} />
            </li>
          ))}
          {favStations.map(stationRow)}
          {!favStations.length && !liveFavs.length && <li className="st-empty">{t('no_favorites')}</li>}
        </ul>
      )}
    </div>
  )
}

export default function StationsView() {
  return (
    <div className="view-body stations-view">
      <StationBrowser full />
    </div>
  )
}
