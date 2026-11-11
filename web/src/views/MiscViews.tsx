import { useEffect, useRef } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { ViewHeader, Markdown } from '../components/common'

export function LogView() {
  const lines = useStore((s) => s.logLines)
  const end = useRef<HTMLDivElement>(null)
  useEffect(() => {
    end.current?.scrollIntoView()
  }, [lines])
  return (
    <>
      <ViewHeader />
      <div className="view-body log-view">
        {lines.map((l, i) => (
          <div key={i} className={l.level}>
            {l.time} {l.level.padEnd(5)} [{l.target}] {l.message}
          </div>
        ))}
        <div ref={end} />
      </div>
    </>
  )
}

export function AboutView() {
  const t = useT()
  const hello = useStore((s) => s.hello)
  return (
    <>
      <ViewHeader />
      <div className="view-body">
        {(hello?.stationName && hello.stationName !== 'MagicDAB') || hello?.description ? (
          <div className="server-desc">
            <div className="section-title">{hello?.stationName}</div>
            {hello?.description && (
              <p>
                <Markdown text={hello.description} />
              </p>
            )}
          </div>
        ) : null}
        <div className="section-title">MagicDAB</div>
        <p>
          {t('version')}: {hello?.version}, © 2026 techkrzysiek
        </p>
        <p>
          Web based DAB/DAB+ receiver written from scratch (OFDM demodulator, FIC/MSC, DAB+, PAD/MOT, TII). MagicDAB is
          free software under the{' '}
          <a href="https://www.gnu.org/licenses/agpl-3.0.html" target="_blank" rel="noreferrer">
            GNU Affero General Public License v3
          </a>{' '}
          or later.
        </p>
        {hello?.sourceUrl && (
          <p>
            {t('source_code')}:{' '}
            <a href={hello.sourceUrl} target="_blank" rel="noreferrer">
              {hello.sourceUrl}
            </a>
          </p>
        )}
        <p className="hint">
          Uses: faad2 (AAC), minimp3 (MP2), libopus, librtlsdr, librdsparser (RDS), React, Leaflet, lucide icons,
          flag-icons (licenses: THIRD-PARTY.md). Map data © OpenStreetMap contributors.
        </p>
      </div>
    </>
  )
}

