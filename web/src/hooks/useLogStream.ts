import { useCallback, useEffect, useRef, useState } from 'react'
import { getApiAuthToken, getApiBaseUrl } from '#/effect/config'

// Live log tail over Server-Sent Events. The backend streams `data:` frames
// (one per log line) and closes a deployment stream with an `event: done`
// frame (src/api/deployments.rs). Service log streams are infinite.
//
// This deliberately bypasses the Effect HTTP client: an infinite body is a
// subscription, not a decode-once request. One stream per mounted view honors
// the backend's process-wide concurrent-stream cap; the AbortController tears
// it down on unmount or when `path` changes.

export type LogStreamStatus =
  | 'idle'
  | 'connecting'
  | 'streaming'
  | 'done'
  | 'error'

export interface LogLine {
  readonly seq: number
  readonly text: string
}

export interface UseLogStream {
  readonly lines: ReadonlyArray<LogLine>
  readonly status: LogStreamStatus
  readonly error: string | null
  readonly clear: () => void
  readonly reconnect: () => void
}

const DEFAULT_MAX = 5000

export function useLogStream(
  path: string | null,
  options?: { readonly max?: number },
): UseLogStream {
  const max = options?.max ?? DEFAULT_MAX
  const [lines, setLines] = useState<ReadonlyArray<LogLine>>([])
  const [status, setStatus] = useState<LogStreamStatus>('idle')
  const [error, setError] = useState<string | null>(null)
  const seqRef = useRef(0)
  const [nonce, setNonce] = useState(0)

  const clear = useCallback(() => {
    seqRef.current = 0
    setLines([])
  }, [])

  const reconnect = useCallback(() => {
    setNonce((n) => n + 1)
  }, [])

  useEffect(() => {
    if (!path) {
      setStatus('idle')
      return
    }

    const controller = new AbortController()
    let cancelled = false
    seqRef.current = 0
    setLines([])
    setError(null)
    setStatus('connecting')

    const push = (text: string) => {
      setLines((prev) => {
        const next = prev.concat({ seq: seqRef.current++, text })
        return next.length > max ? next.slice(next.length - max) : next
      })
    }

    const run = async () => {
      try {
        const token = getApiAuthToken()
        const res = await fetch(`${getApiBaseUrl()}${path}`, {
          headers: {
            accept: 'text/event-stream',
            ...(token ? { authorization: `Bearer ${token}` } : {}),
          },
          signal: controller.signal,
        })
        if (!res.ok || !res.body) {
          if (!cancelled) {
            setError(
              res.status === 401 || res.status === 403
                ? 'Not authorized to stream logs.'
                : `Log stream failed (HTTP ${res.status}).`,
            )
            setStatus('error')
          }
          return
        }
        if (!cancelled) setStatus('streaming')

        const reader = res.body.getReader()
        const decoder = new TextDecoder()
        let buf = ''

        // SSE frame parser. Frames are separated by a blank line; within a
        // frame `event:` names it, `data:` lines accumulate, `:` lines are
        // keep-alive comments. A blank `data:` is a real empty log line.
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buf += decoder.decode(value, { stream: true })
          let idx = buf.indexOf('\n\n')
          while (idx !== -1) {
            const frame = buf.slice(0, idx)
            buf = buf.slice(idx + 2)
            let event = 'message'
            const data: Array<string> = []
            for (const raw of frame.split('\n')) {
              const line = raw.replace(/\r$/, '')
              if (line === '' || line.startsWith(':')) continue
              if (line.startsWith('event:')) event = line.slice(6).trim()
              else if (line.startsWith('data:'))
                data.push(line.slice(5).replace(/^ /, ''))
            }
            if (event === 'done') {
              if (!cancelled) setStatus('done')
              controller.abort()
              return
            }
            if (data.length > 0) push(data.join('\n'))
            idx = buf.indexOf('\n\n')
          }
        }
        if (!cancelled) setStatus((s) => (s === 'streaming' ? 'done' : s))
      } catch (e) {
        if (cancelled || controller.signal.aborted) return
        setError(e instanceof Error ? e.message : String(e))
        setStatus('error')
      }
    }

    void run()

    return () => {
      cancelled = true
      controller.abort()
    }
  }, [path, max, nonce])

  return { lines, status, error, clear, reconnect }
}
