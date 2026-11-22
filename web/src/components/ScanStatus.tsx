import { useEffect, useState } from 'react'
import { useStore } from '../store'
import { useT } from '../i18n'
import type { ScanState } from '../types'

/** Seconds left of what keeps the scan on its channel (a DX recording, the
 *  report of a new multiplex), counted down here between the server's states. */
function useWaitLeft(scan: ScanState | null | undefined) {
  const wait = scan?.wait
  const [until, setUntil] = useState<number | null>(null)
  const [, tick] = useState(0)
  useEffect(() => {
    setUntil(wait ? Date.now() + wait.seconds * 1000 : null)
  }, [wait?.reason, wait?.seconds, scan?.channel])
  useEffect(() => {
    if (until === null) return
    const id = setInterval(() => tick((n) => n + 1), 1000)
    return () => clearInterval(id)
  }, [until])
  return until === null ? null : Math.max(0, Math.ceil((until - Date.now()) / 1000))
}

/** "Scanning 5B (2/38), found: 1", and why it stays: "5B: DX recording, 42 s left". */
export function ScanStatus({ scan }: { scan: ScanState }) {
  const t = useT()
  const left = useWaitLeft(scan)
  return (
    <span>
      {t('scan_running')} {scan.channel} ({scan.index + 1}/{scan.total}), {t('found')}: {scan.found}
      {scan.wait && left !== null && (
        <span className="scan-wait">
          {' '}
          {scan.channel}: {t(`scan_wait_${scan.wait.reason}` as never).replace('{s}', String(left))}
        </span>
      )}
    </span>
  )
}

/** The scan may be stopped here: not one the admin (or the tuning password) started, by a listener without either. */
export function useMayStopScan(scan: ScanState | null | undefined) {
  const privileged = useStore((s) => s.admin || s.tuner)
  return !scan?.privileged || privileged
}
