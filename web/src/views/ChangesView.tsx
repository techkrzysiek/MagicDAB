import { BellOff, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { saveSettings } from '../actions'
import type { Change } from '../types'

function describe(t: (k: string) => string, c: Change) {
  const svc = c.service ? `${c.service}${c.sid ? ` (${c.sid})` : ''}` : ''
  const arrow = c.from !== null && c.to !== null ? `${c.from} → ${c.to}` : c.to ?? c.from ?? ''
  return { title: t('chg_' + c.kind), svc, arrow }
}

interface Muted {
  id: string
  name: string
}
interface NotifyMute {
  ensembles: Muted[]
  services: Muted[]
}

export default function ChangesView() {
  const t = useT()
  const log = useStore((s) => s.changeLog)
  const admin = useStore((s) => s.admin)
  const mute: NotifyMute = useStore((s) => s.settings?.settings?.notifyMute) ?? { ensembles: [], services: [] }
  // the admin chooses multiplexes and services the change log leaves out
  const setMute = (m: NotifyMute) => saveSettings({ notifyMute: m })
  const muteEnsemble = (id: string, name: string) =>
    !mute.ensembles.some((m) => m.id === id) && setMute({ ...mute, ensembles: [...mute.ensembles, { id, name }] })
  const muteService = (id: string, name: string) =>
    !mute.services.some((m) => m.id === id) && setMute({ ...mute, services: [...mute.services, { id, name }] })
  const muted = [...mute.ensembles.map((m) => ({ ...m, kind: 'ensembles' as const })), ...mute.services.map((m) => ({ ...m, kind: 'services' as const }))]
  return (
    <div className="changes">
      {admin && muted.length > 0 && (
        <div className="change-muted">
          <span className="hint">{t('chg_muted')}:</span>
          {muted.map((m) => (
            <span key={m.kind + m.id} className="chip">
              {m.name || m.id} <span className="hint">{m.id}</span>
              <button
                className="icon-btn"
                title={t('chg_unmute')}
                aria-label={t('chg_unmute')}
                onClick={() => setMute({ ...mute, [m.kind]: mute[m.kind].filter((x) => x.id !== m.id) })}
              >
                <X size={12} />
              </button>
            </span>
          ))}
        </div>
      )}
      {!log.length && <div className="hint">{t('no_changes')}</div>}
      {log.map((ev, i) => (
        <div key={i} className="change-ev">
          <div className="change-head">
            <b>{ev.channel}</b> {ev.ensemble} <span className="hint">({ev.eid}) · {new Date(ev.time * 1000).toLocaleString()}</span>
            {admin && (
              <button className="icon-btn change-mute" title={t('chg_mute_mux')} aria-label={t('chg_mute_mux')} onClick={() => muteEnsemble(ev.eid, ev.ensemble)}>
                <BellOff size={14} />
              </button>
            )}
          </div>
          {ev.changes.map((c, j) => {
            const d = describe(t, c)
            return (
              <div key={j} className={'change-item ' + c.kind}>
                <span className="change-kind">{d.title}</span> {d.svc && <b>{d.svc}</b>} <span>{d.arrow}</span>
                {admin && c.sid && (
                  <button className="icon-btn change-mute" title={t('chg_mute_service')} aria-label={t('chg_mute_service')} onClick={() => muteService(c.sid!, c.service ?? '')}>
                    <BellOff size={13} />
                  </button>
                )}
              </div>
            )
          })}
        </div>
      ))}
    </div>
  )
}
