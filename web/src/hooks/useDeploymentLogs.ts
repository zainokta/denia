import { useEffect, useState } from 'react'
import { getToken } from '#/effect/auth-store'

interface LogState {
  readonly lines: ReadonlyArray<string>
  readonly error: string | null
  readonly done: boolean
}

const MAX_LINES = 2000

export function useDeploymentLogs(
  deploymentId: string,
  enabled: boolean,
): LogState {
  const [state, setState] = useState<LogState>({
    lines: [],
    error: null,
    done: false,
  })

  useEffect(() => {
    if (!enabled || deploymentId.length === 0) return
    const controller = new AbortController()
    let cancelled = false

    setState({ lines: [], error: null, done: false })

    const baseUrl =
      typeof import.meta !== 'undefined'
        ? (import.meta.env.VITE_DENIA_API_URL ?? '')
        : ''
    const token = getToken()

    const headers: Record<string, string> = { accept: 'text/event-stream' }
    if (token) headers.authorization = `Bearer ${token}`

    fetch(`${baseUrl}/v1/deployments/${deploymentId}/logs`, {
      headers,
      signal: controller.signal,
    })
      .then(async (response) => {
        if (!response.ok || !response.body) {
          throw new Error(`stream failed: ${response.status}`)
        }
        const reader = response.body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        for (;;) {
          const { value, done } = await reader.read()
          if (done) {
            if (!cancelled) {
              setState((prev) => ({ ...prev, done: true }))
            }
            break
          }
          if (cancelled) return
          buffer += decoder.decode(value, { stream: true })
          const events = buffer.split('\n\n')
          buffer = events.pop() ?? ''
          const fresh: string[] = []
          let sawDone = false
          for (const event of events) {
            let eventName = 'message'
            const dataParts: string[] = []
            for (const line of event.split('\n')) {
              if (line.startsWith('event:')) {
                eventName = line.slice(6).trim()
              } else if (line.startsWith('data:')) {
                dataParts.push(line.slice(5).trimStart())
              }
            }
            if (eventName === 'done') {
              sawDone = true
              continue
            }
            if (dataParts.length > 0) {
              fresh.push(dataParts.join('\n'))
            }
          }
          if (fresh.length > 0) {
            setState((prev) => {
              const next = prev.lines.concat(fresh)
              const trimmed =
                next.length > MAX_LINES ? next.slice(-MAX_LINES) : next
              return { lines: trimmed, error: null, done: prev.done }
            })
          }
          if (sawDone) {
            if (!cancelled) {
              setState((prev) => ({ ...prev, done: true }))
            }
            controller.abort()
            return
          }
        }
      })
      .catch((err: unknown) => {
        if (cancelled || controller.signal.aborted) return
        setState((prev) => ({
          ...prev,
          error: err instanceof Error ? err.message : 'log stream failed',
        }))
      })

    return () => {
      cancelled = true
      controller.abort()
    }
  }, [deploymentId, enabled])

  return state
}
