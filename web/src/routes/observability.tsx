import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useEffect, useRef, useState } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { cpuPercent, formatBytes } from '#/lib/metrics'
import type { CpuCounters } from '#/effect/schema'

const fetchNode = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getNodeMetrics
})

const fetchWorkloads = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listWorkloads
})

export const Route = createFileRoute('/observability')({
  component: ObservabilityRoute,
})

function NodePanel() {
  const samples = useRef<{ cpu: CpuCounters; at: number }[]>([])
  const [cpuPct, setCpuPct] = useState<number | null>(null)

  const { data, isFetching } = useQuery({
    queryKey: ['observability', 'node'],
    queryFn: () => runQuery(fetchNode),
    refetchInterval: 5000,
  })

  useEffect(() => {
    if (!data) return
    samples.current.push({ cpu: data.cpu, at: Date.now() })
    if (samples.current.length > 6) samples.current.shift()
    if (samples.current.length >= 2) {
      const prev = samples.current[0].cpu
      const curr = samples.current[samples.current.length - 1].cpu
      setCpuPct(cpuPercent(prev, curr))
    }
  }, [data])

  if (!data) {
    return (
      <section className="panel p-4 text-sm text-[var(--fg-muted)]">
        {isFetching ? 'loading node metrics…' : 'no data'}
      </section>
    )
  }

  const memUsed = data.memory_total_bytes - data.memory_available_bytes
  const memPct = (memUsed / data.memory_total_bytes) * 100
  const diskUsed = data.disk_total_bytes - data.disk_available_bytes
  const diskPct = (diskUsed / data.disk_total_bytes) * 100

  return (
    <section className="panel overflow-hidden">
      <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
        <p className="kicker m-0">node</p>
        <p className="kicker m-0 ml-auto tnum">
          {new Date(data.recorded_at).toLocaleTimeString()}
        </p>
      </div>
      <dl className="grid grid-cols-2 gap-4 p-4 sm:grid-cols-4">
        <div>
          <dt className="kicker">cpu</dt>
          <dd className="tnum mt-1 text-lg font-semibold text-[var(--fg)]">
            {cpuPct === null ? '—' : `${cpuPct.toFixed(1)}%`}
          </dd>
        </div>
        <div>
          <dt className="kicker">memory</dt>
          <dd className="tnum mt-1 text-lg font-semibold text-[var(--fg)]">
            {memPct.toFixed(1)}%
          </dd>
          <p className="kicker mt-0.5">
            {formatBytes(memUsed)} / {formatBytes(data.memory_total_bytes)}
          </p>
        </div>
        <div>
          <dt className="kicker">disk</dt>
          <dd className="tnum mt-1 text-lg font-semibold text-[var(--fg)]">
            {diskPct.toFixed(1)}%
          </dd>
          <p className="kicker mt-0.5">
            {formatBytes(diskUsed)} / {formatBytes(data.disk_total_bytes)}
          </p>
        </div>
        <div>
          <dt className="kicker">load</dt>
          <dd className="tnum mt-1 text-lg font-semibold text-[var(--fg)]">
            {data.load_1m.toFixed(2)}
          </dd>
          <p className="kicker mt-0.5">
            {data.load_5m.toFixed(2)} · {data.load_15m.toFixed(2)}
          </p>
        </div>
      </dl>
    </section>
  )
}

function WorkloadsPanel() {
  const { data: workloads = [], isFetching } = useQuery({
    queryKey: ['observability', 'workloads'],
    queryFn: () => runQuery(fetchWorkloads),
    refetchInterval: 5000,
  })

  return (
    <section className="panel overflow-hidden">
      <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
        <p className="kicker m-0">workloads</p>
        <p className="kicker m-0 ml-auto">
          {isFetching ? 'fetching…' : `${workloads.length} running`}
        </p>
      </div>
      {workloads.length === 0 ? (
        <p className="px-4 py-6 text-sm text-[var(--fg-muted)]">
          No workloads.
        </p>
      ) : (
        <table className="w-full text-left text-sm">
          <thead>
            <tr className="border-b border-[var(--border)] text-xs text-[var(--fg-muted)]">
              <th className="px-4 py-2 font-semibold">service</th>
              <th className="px-4 py-2 font-semibold">deployment</th>
              <th className="px-4 py-2 font-semibold tnum">cpu</th>
              <th className="px-4 py-2 font-semibold tnum">memory</th>
            </tr>
          </thead>
          <tbody>
            {workloads.map((w, i) => (
              <tr
                key={w.service_id}
                className={i > 0 ? 'border-t border-[var(--border)]' : ''}
              >
                <td className="px-4 py-3 text-xs text-[var(--fg)]">
                  {w.service_name}
                </td>
                <td className="px-4 py-3 text-xs text-[var(--fg-muted)] tnum">
                  {w.deployment_id ? w.deployment_id.slice(0, 8) : '—'}
                </td>
                <td className="px-4 py-3 tnum text-xs text-[var(--fg-muted)]">
                  {w.cpu_usage_usec === null
                    ? '—'
                    : `${(w.cpu_usage_usec / 1_000_000).toFixed(2)}s`}
                </td>
                <td className="px-4 py-3 tnum text-xs text-[var(--fg-muted)]">
                  {w.memory_current_bytes === null
                    ? '—'
                    : formatBytes(w.memory_current_bytes)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}

export function ObservabilityRoute() {
  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">observability</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Node &amp; workloads
      </h1>
      <div className="flex flex-col gap-6">
        <NodePanel />
        <WorkloadsPanel />
      </div>
    </main>
  )
}
