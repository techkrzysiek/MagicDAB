import { useEffect, useMemo, useState } from 'react'
import { ExternalLink, X } from 'lucide-react'
import { useStore, type GallerySlide } from '../store'
import { useT } from '../i18n'
import { ViewHeader, safeUrl } from '../components/common'
import { send } from '../ws'
import { findService } from '../util'

/**
 * Slides of the station seen during this session: grouped by category when it
 * sends a categorised slideshow (CatSLS), otherwise the last slides shown.
 */
export default function SlidesView() {
  const t = useT()
  const selected = useStore((s) => s.selected)
  const label = useStore((s) => findService(s.ensemble, s.selected?.sid)?.label)
  const current = useStore((s) => (s.selected ? s.services[s.selected.sid]?.slide?.url : undefined))
  const gallery = useStore((s) => s.gallery)
  const urls = useStore((s) => s.galleryUrls)
  const [open, setOpen] = useState<GallerySlide | null>(null)
  const sid = selected?.sid

  // the index, and the pictures not here yet, again with every new slide
  useEffect(() => {
    if (sid === undefined) return
    send({ type: 'getGallery', sid, have: Object.keys(useStore.getState().galleryUrls).map(Number) })
  }, [sid, current])

  const slides = gallery && gallery.sid === sid ? gallery.slides : []
  const categories = useMemo(() => {
    const m = new Map<number, { title: string | null; slides: GallerySlide[] }>()
    for (const s of slides) {
      if (s.category === null) continue
      const c = m.get(s.category) ?? { title: null, slides: [] }
      c.title = c.title ?? s.categoryTitle
      c.slides.push(s)
      m.set(s.category, c)
    }
    for (const c of m.values()) c.slides.sort((a, b) => (a.slide ?? 0) - (b.slide ?? 0))
    return [...m.entries()].sort((a, b) => a[0] - b[0])
  }, [slides])
  const recent = useMemo(() => slides.filter((s) => s.category === null).reverse(), [slides])

  const tile = (s: GallerySlide) => (
    <button key={s.id} className="gallery-tile" onClick={() => setOpen(s)} title={s.contentName}>
      {urls[s.id] ? <img src={urls[s.id]} alt={s.contentName} loading="lazy" /> : <div className="gallery-wait" />}
      <span className="hint">{new Date(s.received * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</span>
    </button>
  )

  return (
    <>
      <ViewHeader />
      <div className="view-body gallery">
        {label && <div className="section-title">{label}</div>}
        {!slides.length && <div className="empty">{t('slides_none')}</div>}
        {categories.map(([cat, c]) => (
          <div key={cat}>
            <div className="gallery-cat">{c.title?.trim() || t('slides_category').replace('{n}', String(cat))}</div>
            <div className="gallery-grid">{c.slides.map(tile)}</div>
          </div>
        ))}
        {recent.length > 0 && (
          <div>
            {categories.length > 0 && <div className="gallery-cat">{t('slides_recent')}</div>}
            <div className="gallery-grid">{recent.map(tile)}</div>
          </div>
        )}
      </div>
      {open && (
        <div className="gallery-full" onClick={() => setOpen(null)}>
          <div className="gallery-full-box" onClick={(e) => e.stopPropagation()}>
            <button className="icon-btn gallery-close" onClick={() => setOpen(null)} aria-label="close">
              <X />
            </button>
            {urls[open.id] && <img src={urls[open.id]} alt={open.contentName} />}
            <div className="gallery-caption">
              {open.categoryTitle && <b>{open.categoryTitle.trim()} · </b>}
              <span>{open.contentName}</span>
              <span className="hint"> · {new Date(open.received * 1000).toLocaleString()}</span>
              {safeUrl(open.clickThroughUrl) && (
                <a href={safeUrl(open.clickThroughUrl)} target="_blank" rel="noreferrer" className="gallery-link">
                  <ExternalLink size={14} /> {t('slides_open')}
                </a>
              )}
            </div>
          </div>
        </div>
      )}
    </>
  )
}
