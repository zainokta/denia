import '@xterm/xterm/css/xterm.css'
import { useEffect, useRef, useState } from 'react'
// Type-only import keeps the CommonJS `@xterm/xterm` package out of the SSR
// prerender bundle; the runtime constructor is pulled in via dynamic import
// inside the (client-only) mount effect below.
import type { Terminal } from '@xterm/xterm'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { getApiBaseUrl } from '#/effect/config'
import { runQuery } from '#/effect/runtime'
import { errorMessage } from './ErrorPanel'

function listReplicas(serviceId: string) {
  return Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listConsoleReplicas(serviceId)
  })
}

function createTicket(
  serviceId: string,
  replicaIndex: number,
  cols: number,
  rows: number,
) {
  return Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createConsoleTicket(serviceId, replicaIndex, cols, rows)
  })
}

function wsUrl(path: string): string {
  const base = getApiBaseUrl()
  const origin = base || window.location.origin
  const url = new URL(path, origin)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  return url.toString()
}

export function ServiceConsole({ serviceId }: { readonly serviceId: string }) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const termRef = useRef<Terminal | null>(null)
  const socketRef = useRef<WebSocket | null>(null)
  const [selectedReplica, setSelectedReplica] = useState<number | null>(null)
  const [status, setStatus] = useState('disconnected')
  const [error, setError] = useState('')

  const { data: replicas = [], isLoading } = useQuery({
    queryKey: ['services', serviceId, 'console', 'replicas'],
    queryFn: () => runQuery(listReplicas(serviceId)),
    refetchInterval: 5000,
  })

  useEffect(() => {
    if (selectedReplica !== null || replicas.length !== 1) return
    setSelectedReplica(replicas[0].replica_index)
  }, [replicas, selectedReplica])

  useEffect(() => {
    let disposed = false
    void import('@xterm/xterm').then(({ Terminal }) => {
      if (disposed) return
      const terminal = new Terminal({
        cols: 120,
        rows: 32,
        cursorBlink: true,
        convertEol: true,
        fontFamily: 'JetBrains Mono, ui-monospace, SFMono-Regular, monospace',
        fontSize: 13,
        theme: {
          background: '#121115',
          foreground: '#f4eff7',
          cursor: '#ff4fa3',
        },
      })
      termRef.current = terminal
      if (hostRef.current) terminal.open(hostRef.current)
    })
    return () => {
      disposed = true
      socketRef.current?.close()
      termRef.current?.dispose()
    }
  }, [])

  const connect = useMutation({
    mutationFn: async () => {
      const replica = selectedReplica
      const terminal = termRef.current
      if (replica === null || terminal === null) throw new Error('select a replica')
      const ticket = await runQuery(
        createTicket(serviceId, replica, terminal.cols, terminal.rows),
      )
      return ticket.ws_path
    },
    onSuccess: (path) => {
      const terminal = termRef.current
      if (!terminal) return
      terminal.clear()
      const socket = new WebSocket(wsUrl(path))
      socket.binaryType = 'arraybuffer'
      socketRef.current = socket
      setError('')
      setStatus('connecting')
      socket.onopen = () => {
        setStatus('connected')
        terminal.focus()
      }
      socket.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer) {
          terminal.write(new Uint8Array(event.data))
          return
        }
        if (typeof event.data === 'string' && event.data.includes('"type":"error"')) {
          setError(event.data)
          setStatus('error')
        }
      }
      socket.onclose = () => setStatus('disconnected')
      socket.onerror = () => {
        setStatus('error')
        setError('console websocket failed')
      }
      terminal.onData((data) => {
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(new TextEncoder().encode(data))
        }
      })
    },
    onError: (err: unknown) => {
      setError(errorMessage(err))
      setStatus('error')
    },
  })

  return (
    <div className="stack">
      <div className="panel-head">
        <div className="cluster">
          <label className="kicker" htmlFor="console-replica">
            replica
          </label>
          <select
            id="console-replica"
            className="field-input"
            value={selectedReplica ?? ''}
            onChange={(event) => setSelectedReplica(Number(event.target.value))}
            disabled={isLoading || replicas.length === 0 || status === 'connected'}
          >
            <option value="" disabled>
              select
            </option>
            {replicas.map((replica) => (
              <option key={replica.replica_index} value={replica.replica_index}>
                {replica.replica_index} · {replica.state}
              </option>
            ))}
          </select>
          <span className="badge">{status}</span>
        </div>
        <div className="cluster">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => connect.mutate()}
            disabled={selectedReplica === null || connect.isPending || status === 'connected'}
          >
            connect
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => socketRef.current?.close()}
            disabled={status !== 'connected'}
          >
            disconnect
          </button>
        </div>
      </div>
      {error ? <p className="field-error">{error}</p> : null}
      <div className="terminal-panel" ref={hostRef} aria-label="service console terminal" />
    </div>
  )
}
