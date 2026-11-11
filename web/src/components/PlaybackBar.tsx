import { useEffect, useState } from 'react'
import { History, Radio } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { send } from '../ws'
import { player } from '../audio/player'
import { formatDuration, whenShort } from '../util'

const SEEK_KEYS = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown']

/**
 * A DX recording plays on this listener's own receiver: what it is, where
 * in it (drag to move), back to the air. Shown above every view, so a
 * recording is never taken for the air.
 */
export function PlaybackBar() {
  const t = useT()
  const info = useStore((s) => s.state?.playbackInfo ?? null)
  const position = useStore((s) => s.signal?.position ?? null)
  const duration = useStore((s) => s.signal?.duration ?? s.state?.playbackInfo?.duration ?? null)
  // what the slider shows while dragged, and for a moment after a move (the
  // receiver's position comes with its next signal message)
  const [drag, setDrag] = useState<number | null>(null)
  const [moved, setMoved] = useState<{ to: number } | null>(null)
  useEffect(() => {
    if (!moved) return
    const i = setTimeout(() => setMoved(null), 1500)
    return () => clearTimeout(i)
  }, [moved])
  if (!info) return null
  const commit = (to: number) => {
    setDrag(null)
    setMoved({ to })
    send({ type: 'dxrec', action: 'seek', seconds: to })
    // (what is buffered is the old place)
    player.flush()
  }
  const at = drag ?? moved?.to ?? position ?? 0
  const when = info.time ? whenShort(info.time) : ''
  return (
    <div className="playback-bar">
      <History size={16} className="pb-icon" />
      <span className="pb-what" title={info.name}>
        <b>{t('playback_title')}</b> {[info.label, info.channel, when].filter(Boolean).join(', ')}
      </span>
      {duration ? (
        <>
          <input
            className="pb-seek"
            type="range"
            min={0}
            max={duration}
            step={1}
            value={Math.min(at, duration)}
            aria-label={t('playback_seek')}
            onChange={(e) => setDrag(parseFloat(e.target.value))}
            onPointerUp={(e) => commit(parseFloat((e.target as HTMLInputElement).value))}
            onKeyUp={(e) => {
              if (SEEK_KEYS.includes(e.key)) commit(parseFloat((e.target as HTMLInputElement).value))
            }}
          />
          <span className="pb-time">
            {formatDuration(at)} / {formatDuration(duration)}
          </span>
        </>
      ) : null}
      <button className="btn" onClick={() => send({ type: 'dxrec', action: 'stopPlayback' })} title={t('playback_live_help')}>
        <Radio size={14} /> {t('playback_live')}
      </button>
    </div>
  )
}
