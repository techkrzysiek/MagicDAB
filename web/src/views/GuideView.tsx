import { useEffect, useMemo, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader, Logo } from '../components/common'
import { send } from '../ws'
import { selectService } from '../actions'

export default function GuideView() {
  const t = useT()
  const index = useStore((s) => s.epgIndex)
  const epg = useStore((s) => s.epg)
  const ens = useStore((s) => s.ensemble)
  const list = useStore((s) => s.serviceList)
  const selected = useStore((s) => s.selected)
  const sids = useMemo(() => Object.keys(index).map((h) => parseInt(h, 16)), [index])
  const [sid, setSid] = useState<number | null>(null)
  const cur = sid ?? (selected && sids.includes(selected.sid) ? selected.sid : sids[0] ?? null)
  useEffect(() => {
    if (cur !== null) send({ type: 'getEpg', sid: cur })
  }, [cur])
  const label = (s: number) =>
    ens.services?.find((x) => x.sid === s)?.label ?? list.flatMap((e) => e.services).find((x) => x.sid === s)?.label ?? s.toString(16).toUpperCase()
  const progs = cur !== null ? epg[cur] ?? [] : []
  const now = Date.now() / 1000
  // a schedule whose programmes all ended long ago (sent by the broadcaster as such)
  const outdated = progs.length > 0 && progs.every((p) => p.start + p.duration < now - 86400)
  const thisYear = new Date().getFullYear()
  const byDay = useMemo(() => {
    const m = new Map<string, typeof progs>()
    for (const p of progs) {
      const date = new Date(p.start * 1000)
      const d = date.toLocaleDateString(undefined, {
        weekday: 'long',
        day: '2-digit',
        month: '2-digit',
        ...(date.getFullYear() !== thisYear ? { year: 'numeric' } : {}),
      })
      if (!m.has(d)) m.set(d, [])
      m.get(d)!.push(p)
    }
    return [...m.entries()]
  }, [progs])
  return (
    <>
      <ViewHeader />
      {sids.length === 0 ? (
        <div className="empty">{t('epg_empty')}</div>
      ) : (
        <div className="guide">
          <div className="guide-services">
            {sids.map((s) => (
              <div key={s} className={'svc-item' + (s === cur ? ' selected' : '')} onClick={() => setSid(s)} onDoubleClick={() => selectService(s, 0)}>
                <Logo label={label(s)} sid={s} small />
                <div className="txt">
                  <div className="lbl">{label(s)}</div>
                  <div className="sid">{index[s.toString(16).toUpperCase()]} {t('programmes')}</div>
                </div>
              </div>
            ))}
          </div>
          <div className="guide-list">
            {outdated && (
              <div className="banner update" style={{ textAlign: 'left' }}>
                {t('epg_outdated').replace('{d}', new Date(progs[progs.length - 1].start * 1000).toLocaleDateString())}
              </div>
            )}
            {byDay.map(([day, items]) => (
              <div key={day}>
                <div className="guide-day">{day}</div>
                {items.map((p, i) => {
                  const live = now >= p.start && now < p.start + p.duration
                  return (
                    <div key={i} className={'guide-item' + (live ? ' live' : '') + (p.start + p.duration < now ? ' past' : '')}>
                      <div className="guide-time">
                        {new Date(p.start * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
                        <span className="hint"> {Math.round(p.duration / 60)} min</span>
                      </div>
                      <div>
                        <div className="guide-title">
                          {live && <span className="live-dot">●</span>} {p.name}
                        </div>
                        {p.description && <div className="hint">{p.description}</div>}
                      </div>
                    </div>
                  )
                })}
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  )
}
