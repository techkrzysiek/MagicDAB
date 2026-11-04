import { useEffect, useRef, useState } from 'react'
import { Send, Trash2, X } from 'lucide-react'
import { useStore } from '../store'
import { useT } from '../i18n'
import { chatClear, chatDelete, sendChat, setChatNick, unescapeNick } from '../chat'

/** Chat of the listeners (compatible with the one of FM-DX-Webserver): a
 *  window over the page, so the station stays in view. */
export function ChatPopup() {
  const t = useT()
  const chat = useStore((s) => s.chat)
  const nick = useStore((s) => s.chatNick)
  const [text, setText] = useState('')
  const [nickEdit, setNickEdit] = useState<string | null>(null)
  const list = useRef<HTMLDivElement>(null)
  const box = useRef<HTMLDivElement>(null)
  // (the reader is at the newest message: it stays in view)
  const atEnd = useRef(true)
  const admin = chat.me?.admin === true
  const close = () => useStore.getState().set({ chatOpen: false })
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // the newest message in view
  useEffect(() => {
    const l = list.current
    if (l) l.scrollTop = l.scrollHeight
  }, [chat.messages])

  // phones: the window covers just the part of the screen the keyboard
  // leaves; otherwise the browser pushes the whole page up and down under it
  useEffect(() => {
    const vv = window.visualViewport
    const el = box.current
    if (!vv || !el) return
    const fit = () => {
      el.style.setProperty('--vv-top', `${vv.offsetTop}px`)
      el.style.setProperty('--vv-height', `${vv.height}px`)
    }
    const resized = () => {
      fit()
      const l = list.current
      if (l && atEnd.current) requestAnimationFrame(() => (l.scrollTop = l.scrollHeight))
    }
    fit()
    vv.addEventListener('resize', resized)
    vv.addEventListener('scroll', fit)
    return () => {
      vv.removeEventListener('resize', resized)
      vv.removeEventListener('scroll', fit)
    }
  }, [])

  const send = (e: React.FormEvent) => {
    e.preventDefault()
    const v = text.trim()
    if (!v || !chat.connected) return
    sendChat(v)
    setText('')
  }
  const saveNick = () => {
    if (nickEdit !== null) setChatNick(nickEdit)
    setNickEdit(null)
  }

  return (
    <div className="chat-popup" role="dialog" aria-label={t('nav_chat')} ref={box}>
      <div className="chat-head">
        <b>{t('nav_chat')}</b>
        {/* who I am here: tap to change */}
        {nickEdit === null ? (
          <button className="chat-me" onClick={() => setNickEdit(nick)} title={t('chat_change_nick')}>
            {admin && <span className="chat-admin">ADMIN</span>}
            {nick}
          </button>
        ) : (
          <input
            autoFocus
            className="chat-nick-input"
            value={nickEdit}
            maxLength={32}
            aria-label={t('chat_change_nick')}
            onChange={(e) => setNickEdit(e.target.value)}
            onBlur={saveNick}
            onKeyDown={(e) => {
              if (e.key === 'Enter') saveNick()
              if (e.key === 'Escape') setNickEdit(null)
            }}
          />
        )}
        <span style={{ flex: 1 }} />
        {!chat.connected && <span className="hint">{t('chat_offline')}</span>}
        {admin && (
          <button className="icon-btn" disabled={!chat.messages.length} onClick={() => confirm(t('chat_clear_confirm')) && chatClear()} title={t('chat_clear')} aria-label={t('chat_clear')}>
            <Trash2 size={16} />
          </button>
        )}
        <button className="icon-btn" onClick={close} aria-label="close">
          <X size={20} />
        </button>
      </div>
      <div
        className="chat-list"
        ref={list}
        onScroll={(e) => {
          const l = e.currentTarget
          atEnd.current = l.scrollHeight - l.scrollTop - l.clientHeight < 60
        }}
      >
        {chat.messages.length === 0 && <div className="hint chat-empty">{t('chat_empty')}</div>}
        {chat.messages.map((m) => (
          <div key={m.id} className={'chat-msg' + (m.admin ? ' admin' : '')}>
            <div className="chat-meta">
              {m.admin && <span className="chat-admin">ADMIN</span>}
              <b className="chat-name" title={m.ip ? `IP: ${m.ip}` : undefined}>
                {unescapeNick(m.nickname)}
              </b>
              <span className="chat-time" title={m.time}>
                {m.time.slice(11)}
              </span>
              {admin && (
                <button className="icon-btn chat-del" onClick={() => chatDelete(m.id)} aria-label={t('chat_delete')} title={t('chat_delete')}>
                  <Trash2 size={13} />
                </button>
              )}
            </div>
            <div className="chat-text">{m.message}</div>
          </div>
        ))}
      </div>
      <form className="chat-send" onSubmit={send}>
        <input value={text} maxLength={255} placeholder={t('chat_placeholder')} enterKeyHint="send" onChange={(e) => setText(e.target.value)} />
        <button className="btn primary" type="submit" disabled={!text.trim() || !chat.connected} aria-label={t('chat_send')}>
          <Send size={18} />
        </button>
      </form>
    </div>
  )
}
