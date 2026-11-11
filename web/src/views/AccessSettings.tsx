import { useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { banAddress, unbanAddress } from '../actions'
import { ListenerTable } from '../components/Listeners'
import { Card, Row, Toggle, fieldOk, rule, useField } from '../components/SettingsKit'
import type { Ban } from '../types'

/** Stable fallback for store selectors (a new [] each time loops with zustand 5). */
const EMPTY: never[] = []

/** "AS13335, 8075 as1234" → [13335, 8075, 1234] */
function parseAsns(text: string) {
  const out: number[] = []
  for (const m of text.matchAll(/(?:as\s*)?(\d+)/gi)) {
    const n = parseInt(m[1])
    if (n > 0 && n < 2 ** 32 && !out.includes(n)) out.push(n)
  }
  return out
}

const asnText = (asns: number[] | undefined) => (asns ?? []).map((n) => `AS${n}`).join(', ')

rule('blockedAsns', { ok: (v: string) => /^[\sAa-zA-Z0-9,;]*$/.test(v), encode: parseAsns })
rule('tunePassword', { ok: (v: string) => v === '' || v.length >= 4 })
rule('adminPassword', { ok: (v: string) => v.length >= 4 })

const when = (unix: number) => new Date(unix * 1000).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' })

/** Settings > Access: who may tune, refused networks, bans, the connected listeners, the admin password. */
export default function AccessSettings() {
  const t = useT()
  const s = useStore((st) => st.settings?.settings)
  const access = s?.access
  const tor = useStore((st) => st.settings?.torExits)
  const nets = useStore((st) => st.settings?.refusedNetworks)
  const channels = useStore((st) => st.hello?.channels ?? EMPTY)
  const listeners = useStore((st) => st.listeners)
  const [publicServer, setPublicServer] = useField('publicServer', s?.publicServer ?? true)
  const [locked, setLocked] = useField('tunerLocked', s?.tunerLocked ?? false)
  const [idle, setIdle] = useField('idleCheck', s?.idleCheck ?? { enabled: false, idleMinutes: 60, answerSeconds: 60 })
  // (no password known: a new one, or '' to remove it)
  const [tunePw, setTunePw] = useField<string | null>('tunePassword', null)
  const [allowed, setAllowed] = useField<string[]>('allowedChannels', access?.allowedChannels ?? EMPTY)
  const [blockTor, setBlockTor] = useField('blockTor', !!access?.blockTor)
  const [asns, setAsns] = useField('blockedAsns', asnText(access?.blockedAsns))
  const [hours, setHours] = useField('autoBanHours', access?.autoBanHours ?? 24)
  const [adminPw, setAdminPw] = useField<string | null>('adminPassword', null)
  const [ban, setBan] = useState({ address: '', reason: '', hours: 0 })
  if (!access) return null
  const bans: Ban[] = [...(access.bans ?? [])].sort((a, b) => b.since - a.since)
  const toggle = (c: string) => setAllowed(allowed.includes(c) ? allowed.filter((x) => x !== c) : [...allowed, c])
  const removing = tunePw === ''
  return (
    <>
      <Card title={t('set_card_tuning')}>
        <Row label={t('public_server')} hint={t('public_server_help')}>
          <Toggle checked={publicServer} onChange={setPublicServer} />
        </Row>
        <Row label={t('tuner_lock')} hint={t('set_lock_hint')}>
          <Toggle checked={locked} onChange={setLocked} />
        </Row>
        <Row label={t('tune_pw')} hint={t('tune_pw_help')}>
          <div className="set-inline">
            <input
              type="password"
              autoComplete="new-password"
              className={fieldOk('tunePassword', tunePw ?? '') ? '' : 'invalid'}
              value={tunePw ?? ''}
              placeholder={removing ? t('set_will_remove') : access.tunePasswordSet ? t('tune_pw_is_set') : t('tune_pw_none')}
              onChange={(e) => setTunePw(e.target.value || null)}
            />
            {access.tunePasswordSet && !removing && (
              <button className="btn" onClick={() => setTunePw('')}>
                {t('remove')}
              </button>
            )}
          </div>
        </Row>
        <Row label={t('allowed_channels')} hint={t('allowed_channels_help')} wide>
          <div className="chan-chips">
            <button className={'chan-chip' + (allowed.length ? '' : ' on')} onClick={() => setAllowed([])}>
              {t('allowed_all')}
            </button>
            {channels.map((c) => (
              <button key={c.name} className={'chan-chip' + (allowed.includes(c.name) ? ' on' : '')} onClick={() => toggle(c.name)}>
                {c.name}
              </button>
            ))}
          </div>
        </Row>
      </Card>

      <Card title={t('set_card_blocking')}>
        <Row
          label={t('block_tor')}
          hint={
            access.blockTor
              ? (tor?.count ? t('tor_status').replace('{n}', String(tor.count)).replace('{t}', tor.updated ? when(tor.updated) : '') : t('tor_unavailable')) + (tor?.error ? `: ${tor.error}` : '')
              : undefined
          }
        >
          <Toggle checked={blockTor} onChange={setBlockTor} />
        </Row>
        <Row
          label={t('blocked_asns')}
          hint={
            t('blocked_asns_help') +
            (access.blockedAsns?.length
              ? ' ' + (nets?.asns ? t('asn_status').replace('{p}', String(nets.prefixes)).replace('{n}', String(nets.asns)).replace('{t}', nets.updated ? when(nets.updated) : '') : t('asn_unavailable')) + (nets?.error ? `: ${nets.error}` : '')
              : '')
          }
        >
          <input type="text" className={fieldOk('blockedAsns', asns) ? '' : 'invalid'} placeholder="AS64500, AS64501" value={asns} onChange={(e) => setAsns(e.target.value)} />
        </Row>
        <Row label={t('auto_ban')} hint={t('auto_ban_help')}>
          <input type="number" className="num" min={0} max={8760} value={hours} onChange={(e) => setHours(Math.max(0, Math.min(8760, parseInt(e.target.value) || 0)))} />
        </Row>
      </Card>

      <Card title={t('set_card_idle')}>
        <Row label={t('idle_check')} hint={t('idle_check_help')}>
          <Toggle checked={!!idle.enabled} onChange={(v) => setIdle({ ...idle, enabled: v })} />
        </Row>
        <Row label={t('idle_check_minutes')}>
          <input
            type="number"
            className="num"
            min={1}
            max={1440}
            disabled={!idle.enabled}
            value={idle.idleMinutes}
            onChange={(e) => setIdle({ ...idle, idleMinutes: Math.max(1, Math.min(1440, parseInt(e.target.value) || 1)) })}
          />
        </Row>
        <Row label={t('idle_check_seconds')}>
          <input
            type="number"
            className="num"
            min={10}
            max={3600}
            disabled={!idle.enabled}
            value={idle.answerSeconds}
            onChange={(e) => setIdle({ ...idle, answerSeconds: Math.max(10, Math.min(3600, parseInt(e.target.value) || 10)) })}
          />
        </Row>
      </Card>

      <Card title={`${t('bans')} (${bans.length})`} note={t('set_bans_note')}>
        <div className="set-row wide">
          <form
            className="ban-add"
            onSubmit={(e) => {
              e.preventDefault()
              if (!ban.address.trim()) return
              banAddress(ban.address.trim(), ban.reason, ban.hours)
              setBan({ address: '', reason: '', hours: ban.hours })
            }}
          >
            <input className="input" type="text" placeholder={t('ban_address')} value={ban.address} onChange={(e) => setBan({ ...ban, address: e.target.value })} />
            <input className="input" type="text" maxLength={200} placeholder={t('ban_reason')} value={ban.reason} onChange={(e) => setBan({ ...ban, reason: e.target.value })} />
            <select className="input" value={ban.hours} onChange={(e) => setBan({ ...ban, hours: parseInt(e.target.value) })}>
              <option value={0}>{t('ban_forever')}</option>
              <option value={1}>{t('dur_1h')}</option>
              <option value={24}>{t('dur_24h')}</option>
              <option value={168}>{t('dur_7d')}</option>
              <option value={720}>{t('dur_30d')}</option>
            </select>
            <button type="submit" className="btn danger" disabled={!ban.address.trim()}>
              {t('ban')}
            </button>
          </form>
          <div className="table-wrap">
            <table className="table bans-table">
              <thead>
                <tr>
                  <th>{t('address')}</th>
                  <th>{t('ban_reason_col')}</th>
                  <th>{t('since')}</th>
                  <th>{t('ban_until')}</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {bans.map((b) => (
                  <tr key={b.address}>
                    <td className="mono">{b.address}</td>
                    <td>
                      {b.auto && <span className="chip">{t('ban_auto')}</span>} {b.reason}
                    </td>
                    <td className="nowrap">{when(b.since)}</td>
                    <td className="nowrap">{b.until ? when(b.until) : t('ban_forever')}</td>
                    <td>
                      <button className="btn" onClick={() => unbanAddress(b.address)}>
                        {t('ban_lift')}
                      </button>
                    </td>
                  </tr>
                ))}
                {!bans.length && (
                  <tr>
                    <td colSpan={5} className="hint">
                      {t('no_bans')}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </Card>

      <Card title={`${t('clients')} (${listeners})`}>
        <div className="set-row wide">
          <ListenerTable />
        </div>
      </Card>

      <Card title={t('set_card_admin')}>
        <Row label={t('new_password')} hint={t('set_admin_pw_hint')}>
          <input
            type="password"
            autoComplete="new-password"
            className={adminPw === null || fieldOk('adminPassword', adminPw) ? '' : 'invalid'}
            value={adminPw ?? ''}
            onChange={(e) => setAdminPw(e.target.value || null)}
          />
        </Row>
      </Card>
    </>
  )
}
