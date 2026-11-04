import { useEffect } from 'react'
import { X } from 'lucide-react'
import { useStore } from '../store'
import type { Store } from '../store'
import { useT } from '../i18n'
import { requestLogo } from '../ws'
import type { LogoVariant } from '../types'

/** Pick the best logo variant for a target display size. */
export function pickLogo(variants: LogoVariant[] | undefined, size: number): LogoVariant | undefined {
  if (!variants || variants.length === 0) return undefined
  const squares = variants.filter((v) => v.width === v.height && v.width > 0)
  const pool = squares.length ? squares : variants
  // smallest variant that is at least 2x the display size, otherwise the largest
  const sorted = [...pool].sort((a, b) => a.width - b.width)
  return sorted.find((v) => v.width >= size * 2) ?? sorted[sorted.length - 1]
}

/** The logo in place of a slideshow: the 320×240 one SPI has for it (the
 * broadcast one first), otherwise the largest at least 240 high. */
export function pickSlideLogo(variants: LogoVariant[] | undefined): LogoVariant | undefined {
  if (!variants || variants.length === 0) return undefined
  const slide = variants.filter((v) => v.width === 320 && v.height === 240)
  if (slide.length) return slide.find((v) => v.source === 'SPI') ?? slide[0]
  const largest = [...variants].sort((a, b) => b.width * b.height - a.width * a.height)[0]
  return largest.height >= 240 ? largest : undefined
}

export function useLogoUrl(sid: number | undefined, size: number, ecc?: number | null): string | null {
  return useLogo(sid, ecc, (variants) => pickLogo(variants, size))
}

export function useSlideLogoUrl(sid: number | undefined, ecc?: number | null): string | null {
  return useLogo(sid, ecc, pickSlideLogo)
}

/** The ECC of a service of the multiplex received now: its own, else the multiplex's. */
function currentEcc(s: Store, sid: number | undefined): number | null {
  const svc = sid === undefined ? undefined : s.ensemble.services?.find((x) => x.sid === sid)
  return svc ? (svc.ecc ?? s.ensemble.ecc ?? null) : null
}

/** The logos of a station. The same SId with another ECC is another station (another
 * country), so its logos are not this one's; logos without an ECC fit any. */
export function useLogoVariants(sid: number | undefined, ecc?: number | null): LogoVariant[] | undefined {
  const all = useStore((s) => (sid !== undefined ? s.logos[sid.toString(16).toUpperCase()] : undefined))
  const useRadioDns = useStore((s) => s.useRadioDns)
  const own = useStore((s) => currentEcc(s, sid))
  const want = ecc ?? own
  return all?.filter((v) => (useRadioDns || v.source !== 'RadioDNS') && (!want || !v.ecc || v.ecc === want))
}

function useLogo(sid: number | undefined, ecc: number | null | undefined, pick: (variants: LogoVariant[] | undefined) => LogoVariant | undefined): string | null {
  const v = pick(useLogoVariants(sid, ecc))
  const url = useStore((s) => (v ? s.logoUrls[v.id] : undefined))
  useEffect(() => {
    if (v && !url) requestLogo(v.id)
  }, [v, url])
  return url ?? null
}

function LogoImage({ v }: { v: LogoVariant }) {
  const url = useStore((s) => s.logoUrls[v.id])
  useEffect(() => {
    if (!url) requestLogo(v.id)
  }, [v.id, url])
  return url ? <img src={url} alt="" style={{ maxWidth: Math.min(v.width || 128, 320), maxHeight: 240, imageRendering: v.width <= 64 ? 'pixelated' : 'auto' }} /> : <span className="hint">…</span>
}

/** Dialog with all logos of a service (size, source, file size, reception time). */
export function LogoDialog() {
  const t = useT()
  const sid = useStore((s) => s.logoDialog)
  const set = useStore((s) => s.set)
  const variants = useLogoVariants(sid ?? undefined)
  const ecc = useStore((s) => (sid !== null ? currentEcc(s, sid) : null))
  const text = useStore((s) => {
    if (sid === null) return undefined
    const key = sid.toString(16).padStart(4, '0')
    // (the station of this ECC; the same SId elsewhere is another station)
    return s.texts[(ecc ?? 0).toString(16).padStart(2, '0') + key] ?? (ecc ? undefined : Object.entries(s.texts).find(([k]) => k.endsWith(key))?.[1])
  })
  const label = useStore((s) => s.ensemble.services?.find((x) => x.sid === sid)?.label)
  if (sid === null) return null
  return (
    <div className="modal-back" onClick={() => set({ logoDialog: null })}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <b>{label ?? sid.toString(16).toUpperCase()}</b>
          <span className="hint"> · {t('logos')}</span>
          <span style={{ flex: 1 }} />
          <button className="icon-btn" onClick={() => set({ logoDialog: null })}>
            <X />
          </button>
        </div>
        {text && (text.longName || text.description) && (
          <div className="hint" style={{ marginBottom: 10 }}>
            {text.longName ?? text.mediumName}
            {text.description ? ` – ${text.description}` : ''}
          </div>
        )}
        {!variants?.length && <div className="hint">{t('no_logos')}</div>}
        <div className="logo-grid">
          {variants?.map((v) => (
            <div key={v.id} className="logo-card">
              <div className="logo-preview">
                <LogoImage v={v} />
              </div>
              <div>
                <b>
                  {v.width}×{v.height}
                </b>{' '}
                <span className="hint">{v.mime.replace('image/', '').toUpperCase()}</span>
              </div>
              <div className="hint">
                {(v.size / 1024).toFixed(1)} kB · <span className={'src-tag ' + (v.source === 'SPI' ? 'air' : 'net')}>{v.source === 'SPI' ? 'SPI (DAB)' : 'RadioDNS'}</span>
              </div>
              <div className="hint">{new Date(v.received * 1000).toLocaleString()}</div>
              <div className="hint" style={{ wordBreak: 'break-all' }}>{v.url}</div>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
