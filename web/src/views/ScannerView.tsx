import { useStore, fixedChannelOf } from '../store'
import { useT } from '../i18n'
import { ViewHeader } from '../components/common'
import { TuneLockButton } from '../components/TuneDialog'
import { startScan, stopScan, selectService, tune } from '../actions'
import { send } from '../ws'
import { ago, downloadText, hex, mhz } from '../util'
import { useState } from 'react'
import DxScanner from './DxScanner'
import ChangesView from './ChangesView'
import DxRecordings from './DxRecordings'

export default function ScannerView() {
  const t = useT()
  const list = useStore((s) => s.serviceList)
  const scan = useStore((s) => s.state?.scan)
  const fixed = useStore(fixedChannelOf)
  const admin = useStore((s) => s.admin)
  const current = useStore((s) => s.ensemble)
  const total = list.reduce((a, e) => a + e.services.length, 0)
  const [tab, setTab] = useState<'band' | 'dx' | 'changes' | 'recordings'>('band')
  const nChanges = useStore((s) => s.changeLog.length)
  const recording = useStore((s) => !!s.dxrec?.active)

  const csv = () =>
    [
      'Channel;Frequency;EId;ECC;Label;Services;SNR;TII;LastSeen',
      ...list.map((e) =>
        [e.channel, e.frequency, hex(e.eid), hex(e.ecc, 2), e.label, e.services.length, e.snr.toFixed(1), e.tii.join(' '), new Date(e.lastSeen * 1000).toISOString()].join(';'),
      ),
    ].join('\n')

  return (
    <>
      <ViewHeader />
      <div className="settings-tabs">
        <button className={'tab' + (tab === 'band' ? ' active' : '')} onClick={() => setTab('band')}>
          {t('band_tab')}
        </button>
        <button className={'tab' + (tab === 'dx' ? ' active' : '')} onClick={() => setTab('dx')}>
          {t('dx_scanner')}
        </button>
        <button className={'tab' + (tab === 'changes' ? ' active' : '')} onClick={() => setTab('changes')}>
          {t('changes')} ({nChanges})
        </button>
        {admin && (
          <button className={'tab' + (tab === 'recordings' ? ' active' : '')} onClick={() => setTab('recordings')}>
            {recording ? '● ' : ''}
            {t('dxrec_tab')}
          </button>
        )}
      </div>
      {tab === 'recordings' && admin ? (
        <div className="view-body">
          <DxRecordings />
        </div>
      ) : tab === 'changes' ? (
        <div className="view-body">
          <ChangesView />
        </div>
      ) : tab === 'dx' ? (
        <div className="view-body">
          <DxScanner />
        </div>
      ) : (
      <div className="view-body">
        <div className="section-title">{t('band_scan')}</div>
        <div style={{ display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap', marginBottom: 12 }}>
          {scan && !scan.dx ? (
            <>
              <button className="btn danger" onClick={stopScan}>
                {t('stop_scan')}
              </button>
              <span>
                {t('scan_running')}: {scan.channel} ({scan.index + 1}/{scan.total}), {t('found')}: {scan.found}
              </span>
            </>
          ) : (
            <button className="btn primary" onClick={startScan} disabled={!!fixed} title={fixed ? t('channel_fixed') : undefined}>
              {t('start_scan')}
            </button>
          )}
          {!fixed && <TuneLockButton />}
          <span className="hint">
            {list.length} {t('ensembles').toLowerCase()}, {total} {t('services').toLowerCase()}
          </span>
          <span style={{ flex: 1 }} />
          <button className="btn" onClick={() => downloadText('magicdab-ensembles.csv', csv())} disabled={!list.length}>
            {t('export_csv')}
          </button>
          {admin && (
            <button className="btn" onClick={() => confirm(t('clear_list') + '?') && send({ type: 'serviceList', action: 'clear' })}>
              {t('clear_list')}
            </button>
          )}
        </div>
        <p className="hint">{t('idle_info')}</p>
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th>{t('channel')}</th>
                <th>{t('frequency')}</th>
                <th>EId</th>
                <th>{t('ensemble')}</th>
                <th>{t('services')}</th>
                <th>SNR</th>
                <th>TII</th>
                <th>{t('last_seen')}</th>
                {admin && <th />}
              </tr>
            </thead>
            <tbody>
              {list.map((e) => (
                <tr key={e.channel + e.eid} className={current.valid && current.eid === e.eid && current.channel === e.channel ? 'sel' : ''}>
                  <td onClick={() => tune(e.channel)}>{e.channel}</td>
                  <td onClick={() => tune(e.channel)}>{mhz(e.frequency)} MHz</td>
                  <td>
                    {hex(e.ecc, 2)}
                    {hex(e.eid)}
                  </td>
                  <td onClick={() => e.services[0] && selectService(e.services[0].sid, 0, e.channel)}>
                    <b>{e.label}</b> <span className="hint">{e.short}</span>
                  </td>
                  <td>{e.services.length}</td>
                  <td>{e.snr ? `${e.snr.toFixed(1)} dB` : ''}</td>
                  <td>{e.tii.join(', ')}</td>
                  <td>{ago(e.lastSeen)}</td>
                  {admin && (
                    <td>
                      <button className="btn" onClick={() => send({ type: 'serviceList', action: 'remove', channel: e.channel, eid: e.eid })}>
                        {t('remove')}
                      </button>
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
      )}
    </>
  )
}
