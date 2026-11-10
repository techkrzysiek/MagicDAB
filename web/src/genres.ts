// Names of the TV-Anytime content genres (ContentCS of ETSI TS 102 822-3-1,
// the 2011 edition as the DVB Project publishes it), the genres of SPI
// documents: genres_table.ts, made by build/genres.py (the receiver has the
// same: crates/dab/src/genres.rs).
import { CONTENT } from './genres_table'

/** A genre as shown: a ContentCS URN or code by its name, one not in the
 *  table by the nearest level above it that is ("Era 3.6.16.8"); a name
 *  and other schemes as they are. */
export function genreLabel(genre: string): string {
  const code = (genre.split(':').pop() ?? genre).trim()
  const content = genre.includes('ContentCS') || (!genre.includes(':') && code.startsWith('3.'))
  if (!content || !/^[\d.]+$/.test(code)) return code
  if (CONTENT[code]) return CONTENT[code]
  for (let up = code; up.includes('.'); ) {
    up = up.slice(0, up.lastIndexOf('.'))
    if (CONTENT[up]) return `${CONTENT[up]} ${code}`
  }
  return code
}

/** The levels above it and the genre itself ("Music › Pop-rock › Rock"). */
export function genrePath(genre: string): string {
  const code = (genre.split(':').pop() ?? genre).trim()
  const parts: string[] = []
  for (let up = code; up; up = up.includes('.') ? up.slice(0, up.lastIndexOf('.')) : '') {
    if (CONTENT[up]) parts.unshift(CONTENT[up])
  }
  return parts.join(' › ')
}
