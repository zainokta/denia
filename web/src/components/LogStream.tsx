import { useEffect, useRef, useState } from 'react'
import { Download, RotateCw } from 'lucide-react'
import { useLogStream, type LogStreamStatus } from '#/hooks/useLogStream'
import { CopyButton } from './CopyButton'
import { EmptyState } from './EmptyState'

const STATUS_META: Record<
  LogStreamStatus,
  { readonly label: string; readonly cls: string }
> = {
  idle: { label: 'idle', cls: 'badge' },
  connecting: { label: 'connecting', cls: 'badge badge-warn' },
  streaming: { label: 'live', cls: 'badge badge-steady' },
  done: { label: 'ended', cls: 'badge' },
  error: { label: 'error', cls: 'badge badge-fault' },
}

// Live log viewer over the SSE hook. Autoscrolls while following; pauses follow
// when the operator scrolls up, resumes when they return to the bottom. New
// lines announce politely. Buffer is capped by the hook.
export function LogStream({
  path,
  title,
  height = '26rem',
  showLineNumbers = false,
  max,
}: {
  readonly path: string | null
  readonly title?: string
  readonly height?: string
  readonly showLineNumbers?: boolean
  readonly max?: number
}) {
  const { lines, status, error, reconnect } = useLogStream(path, { max })
  const bodyRef = useRef<HTMLDivElement | null>(null)
  const [follow, setFollow] = useState(true)

  useEffect(() => {
    if (!follow) return
    const el = bodyRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [lines, follow])

  const onScroll = () => {
    const el = bodyRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24
    setFollow(atBottom)
  }

  const meta = STATUS_META[status]
  const allText = lines.map((l) => l.text).join('\n')

  const onDownload = () => {
    const blob = new Blob([allText], { type: 'text/plain' })
    const href = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = href
    a.download = `${title ?? 'logs'}.txt`
    a.click()
    URL.revokeObjectURL(href)
  }

  return (
    <div className="logstream" style={{ ['--logstream-h' as string]: height }}>
      <div className="logstream-head">
        {title ? <span className="kicker">{title}</span> : null}
        <span className={`${meta.cls} logstream-status`}>
          {status === 'streaming' ? (
            <span className="signal signal-steady" aria-hidden="true" />
          ) : null}
          {meta.label}
        </span>
        <label className="cluster" style={{ gap: '0.35rem', fontSize: 'var(--text-label)' }}>
          <input
            type="checkbox"
            className="field-check"
            checked={follow}
            onChange={(e) => setFollow(e.target.checked)}
          />
          follow
        </label>
        <CopyButton value={allText} label="Copy logs" />
        <button type="button" className="btn btn-icon" onClick={onDownload} aria-label="Download logs">
          <Download size={14} aria-hidden="true" />
        </button>
        {(status === 'error' || status === 'done') && path ? (
          <button type="button" className="btn btn-icon" onClick={reconnect} aria-label="Reconnect">
            <RotateCw size={14} aria-hidden="true" />
          </button>
        ) : null}
      </div>
      <div
        ref={bodyRef}
        className="logstream-body"
        onScroll={onScroll}
        aria-live="polite"
        aria-label={title ?? 'Logs'}
      >
        {error ? (
          <p className="field-error">{error}</p>
        ) : lines.length === 0 ? (
          status === 'connecting' ? (
            <p className="text-faint">Connecting…</p>
          ) : (
            <EmptyState title="No output yet" hint="Logs will appear here as they are produced." />
          )
        ) : (
          lines.map((l) => (
            <span key={l.seq} className="logstream-line">
              {showLineNumbers ? <span className="ln">{l.seq + 1}</span> : null}
              {l.text || ' '}
            </span>
          ))
        )}
      </div>
    </div>
  )
}
