import { useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import SpiView from './SpiView'
import TrafficView from './TrafficView'

type Tab = 'spi' | 'traffic'

/** The data services of the multiplex in one view: SPI (logos, programme
 *  information) and, where the multiplex has TPEG, the traffic messages. */
export default function DataView() {
  const t = useT()
  // (a TPEG service on the multiplex: UA 0x004)
  const hasTraffic = useStore((s) => !!s.ensemble.services?.some((x) => x.components.some((c) => c.userApps.some((u) => u.type === 0x004))))
  const count = useStore((s) => Object.keys(s.tpeg).length)
  const [tab, setTabState] = useState<Tab>(() => {
    try {
      return localStorage.getItem('magicdab.dataTab') === 'traffic' ? 'traffic' : 'spi'
    } catch {
      return 'spi'
    }
  })
  const setTab = (v: Tab) => {
    setTabState(v)
    try {
      localStorage.setItem('magicdab.dataTab', v)
    } catch {
      // (not kept)
    }
  }
  // (the traffic tab is always there: greyed out where the multiplex has no TPEG)
  const shown: Tab = tab
  return (
    <div className="data-view">
      <div className="tabs data-tabs">
        <button className={'tab' + (shown === 'spi' ? ' active' : '')} onClick={() => setTab('spi')}>
          SPI
        </button>
        <button
          className={'tab' + (shown === 'traffic' ? ' active' : '') + (hasTraffic ? '' : ' off')}
          onClick={() => setTab('traffic')}
          title={hasTraffic ? undefined : t('traffic_no_tpeg')}
        >
          {t('nav_traffic')}
          {hasTraffic && count > 0 ? ` (${count})` : ''}
        </button>
      </div>
      <div className="data-body">
        {shown === 'spi' ? <SpiView /> : hasTraffic ? <TrafficView /> : <div className="data-empty hint">{t('traffic_no_tpeg')}</div>}
      </div>
    </div>
  )
}
