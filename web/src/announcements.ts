// Announcements (FIG 0/18, 0/19): the station a listener left to hear one,
// and the way back to it when the announcement is over (like TA in a car radio,
// but only after a tap on "Listen").
import { useEffect } from 'react'
import { useStore } from './store'
import { selectService } from './actions'

/** Where to go back: the station before, and the cluster of the announcement. */
let back: { sid: number; scids: number; label: string; cluster: number } | null = null

export function listenToAnnouncement(sourceSid: number, cluster: number, from: { sid: number; scids: number; label: string }) {
  back = { ...from, cluster }
  selectService(sourceSid, 0)
}

/** The station to go back to while an announcement plays (for the banner). */
export function announcementBack() {
  return back
}

export function goBack() {
  const b = back
  back = null
  if (b) selectService(b.sid, b.scids)
}

/** Back to the station when the announcement ends, or forget it when the listener goes elsewhere. */
export function useAnnouncementReturn() {
  const anns = useStore((s) => s.ensemble.announcements)
  const selected = useStore((s) => s.selected)
  useEffect(() => {
    if (!back) return
    if (!(anns ?? []).some((a) => a.cluster === back!.cluster)) goBack()
  }, [anns])
  useEffect(() => {
    // (another station chosen meanwhile: no way back any more)
    if (back && selected && selected.sid === back.sid && selected.scids === back.scids) back = null
  }, [selected])
}
