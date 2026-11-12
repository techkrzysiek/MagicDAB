// How far an update of the Android app is (android/…/Updater.kt, the event
// magicdab-apk in android.ts): the download with its bar and speed, the check,
// Android's question, or why it failed.
import { useStore } from '../store'
import { useT } from '../i18n'

export type ApkProgressState = {
  state: 'downloading' | 'checking' | 'installing' | 'failed' | 'idle'
  version: string
  done: number
  total: number
  /** bytes per second */
  speed: number
  error: string | null
}

const mb = (b: number) => (b / 1048576).toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })
const rate = (b: number) =>
  b >= 1048576 ? `${(b / 1048576).toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} MB/s` : `${Math.round(b / 1024)} kB/s`

/** True while the app works on an update (the install buttons wait then). */
export function useApkBusy(): boolean {
  const a = useStore((s) => s.apk)
  return !!a && (a.state === 'downloading' || a.state === 'checking' || a.state === 'installing')
}

/** The line (and bar) of the update in progress; nothing when there is none. */
export function ApkProgress() {
  const t = useT()
  const a = useStore((s) => s.apk)
  if (!a || a.state === 'idle') return null
  if (a.state === 'failed') return <span className="bad">{t('apk_failed').replace('{e}', a.error ?? '?')}</span>
  if (a.state === 'checking') return <span>{t('apk_checking')}</span>
  if (a.state === 'installing') return <span>{t('apk_installing').replace('{v}', a.version)}</span>
  const known = a.total > 0
  return (
    <div className="apk-progress">
      <span>
        {t(known ? 'apk_downloading' : 'apk_downloading_size')
          .replace('{v}', a.version)
          .replace('{done}', mb(a.done))
          .replace('{total}', known ? mb(a.total) : '')
          .replace('{speed}', a.speed > 0 ? rate(a.speed) : '…')}
      </span>
      <div className="progress">
        <div style={{ width: `${known ? Math.min(100, (a.done / a.total) * 100) : 0}%` }} />
      </div>
    </div>
  )
}
