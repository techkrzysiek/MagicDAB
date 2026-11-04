import { useEffect, useState } from 'react'
import { Play, Users, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { send } from '../ws'
import { banAddress, kickAddress } from '../actions'
import type { Listener } from '../types'

const pad = (n: number) => n.toString().padStart(2, '0')

/** "14:05 (2 h 10 min)": when a listener came and how long ago. */
function since(secs: number) {
  const d = new Date(Date.now() - secs * 1000)
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  return `${pad(d.getHours())}:${pad(d.getMinutes())} (${h ? `${h} h ${m} min` : `${m} min`})`
}

/** The connected listeners for admins, refreshed while shown: kick or ban one. */
export function ListenerTable() {
  const t = useT()
  const list = useStore((s) => s.listenerList)
  const count = useStore((s) => s.listeners)
  // the reason of a ban being written (address)
  const [banning, setBanning] = useState<string | null>(null)
  const [reason, setReason] = useState('')
  useEffect(() => {
    send({ type: 'listeners' })
    const i = setInterval(() => send({ type: 'listeners' }), 5000)
    return () => clearInterval(i)
  }, [count])
  const clients = list?.clients ?? []
  const own = clients.find((c) => c.id === list?.self)?.address
  // the reason of a ban: a row of its own under the listener (the table stays narrow)
  const banRow = (c: Listener) => (
    <tr key={c.id + '-ban'} className="ban-row">
      <td />
      <td colSpan={6}>
        <form
          className="ban-inline"
          onSubmit={(e) => {
            e.preventDefault()
            banAddress(c.address, reason)
            setBanning(null)
          }}
        >
          <input autoFocus className="input" type="text" maxLength={200} placeholder={t('ban_reason')} value={reason} onChange={(e) => setReason(e.target.value)} />
          <button type="submit" className="btn danger">
            {t('ban')} {c.address}
          </button>
          <button type="button" className="btn" onClick={() => setBanning(null)}>
            {t('cancel')}
          </button>
        </form>
      </td>
    </tr>
  )
  const row = (c: Listener) => [
    <tr key={c.id}>
      <td className="hint">#{c.id}</td>
      <td>
        <div className="mono">{c.address}</div>
        <div className="hint">
          {c.asn?.asns.length ? `AS${c.asn.asns.join(', AS')}${c.asn.country ? ` · ${c.asn.country}` : ''}` : ''}
          {c.via ? `${c.asn?.asns.length ? ' · ' : ''}${c.via}` : ''}
        </div>
      </td>
      <td>{c.agent}</td>
      <td className="nowrap">{since(c.connectedSecs)}</td>
      <td>
        {c.audio && <Play size={12} className="listening" aria-label={t('play')} />} {c.station ?? ''}
        {c.channel && <span className="hint"> · {c.channel}</span>}
      </td>
      <td>
        {c.admin && <span className="chip">{t('admin')}</span>}
        {c.tuner && <span className="chip">{t('tuner_badge')}</span>}
        {c.gated && <span className="chip">{t('gated_badge')}</span>}
        {c.id === list?.self && <span className="chip">{t('you')}</span>}
      </td>
      <td className="nowrap">
        {c.address !== own && (
          <>
            <button className="btn" onClick={() => kickAddress(c.address)} title={t('kick_help')}>
              {t('kick')}
            </button>{' '}
            <button
              className="btn danger"
              disabled={banning === c.address}
              onClick={() => {
                setReason('')
                setBanning(c.address)
              }}
            >
              {t('ban')}
            </button>
          </>
        )}
      </td>
    </tr>,
    // (once, under the first connection of the address)
    banning === c.address && clients.find((x) => x.address === c.address) === c ? banRow(c) : null,
  ]
  return (
    <div className="table-wrap">
      <table className="table listeners-table">
        <thead>
          <tr>
            <th />
            <th>{t('address')}</th>
            <th>{t('browser')}</th>
            <th>{t('since')}</th>
            <th>{t('station')}</th>
            <th />
            <th />
          </tr>
        </thead>
        <tbody>{clients.flatMap(row)}</tbody>
      </table>
    </div>
  )
}

/** The listeners from the status bar (admins). */
export function ListenersDialog() {
  const t = useT()
  const open = useStore((s) => s.listenersOpen && s.admin)
  const count = useStore((s) => s.listeners)
  const set = useStore((s) => s.set)
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && set({ listenersOpen: false })
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, set])
  if (!open) return null
  return (
    <div className="modal-back" onClick={() => set({ listenersOpen: false })}>
      <div className="modal listeners-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <Users size={18} />
          <b>
            {t('listeners')} ({count})
          </b>
          <span style={{ flex: 1 }} />
          <button className="icon-btn" onClick={() => set({ listenersOpen: false })} aria-label="close">
            <X />
          </button>
        </div>
        <ListenerTable />
        <p className="hint">{t('listeners_help')}</p>
      </div>
    </div>
  )
}
