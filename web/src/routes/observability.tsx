import { createFileRoute, Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useEffect, useRef, useState } from 'react'
import { Activity, Boxes } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { NodeSnapshot, WorkloadView } from '#/effect/schema'
import { BarMeter, RadialGauge, Sparkline } from '#/components/Charts'
import { StatusBadge } from '#/components/StatusBadge'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { Num } from '#/components/Num'
import type { SemState } from '#/lib/status'
import { cpuBusyTotal, cpuPercentDelta } from '#/lib/metrics'
import { formatBytes, formatClock, formatPercent } from '#/lib/format'

const getNodeMetrics = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getNodeMetrics
})

const listWorkloads = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listWorkloads
})

export const Route = createFileRoute('/observability')({
  component: ObservabilityRoute,
})

// Threshold colouring: utilisation reads steady until it climbs, then warns,
// then faults. Signal, not decoration.
function usageState(pct: number): SemState {
  if (pct >= 88) return 'fault'
  if (pct >= 70) return 'warn'
  return 'steady'
}

// Instantaneous node CPU% from deltas between successive snapshots (the raw
// counters are cumulative since boot). Keeps a short client-side history so the
// gauge reads "now" and the sparkline shows the recent trend. Delta math lives
// in lib/metrics so the dashboard and observability route share one source.
function useNodeHistory(snapshot: NodeSnapshot | undefined) {
  const [series, setSeries] = useState<ReadonlyArray<number>>([])
  const prev = useRef<{ busy: number; total: number; at: string } | null>(null)

  useEffect(() => {
    if (!snapshot) return
    const { busy, total } = cpuBusyTotal(snapshot.cpu)
    const last = prev.current
    if (last && snapshot.recorded_at !== last.at) {
      const pct = cpuPercentDelta(last, { busy, total })
      setSeries((s) => [...s, pct].slice(-40))
    }
    prev.current = { busy, total, at: snapshot.recorded_at }
  }, [snapshot])

  const cpuPct = series.length > 0 ? series[series.length - 1] : 0
  return { cpuPct, cpuSeries: series }
}

// Per-workload CPU% derived from the delta in each service's cumulative
// `cpu_usage_usec` over the wall-clock elapsed between two `/workloads` polls
// (the backend exposes the raw counter, not a percentage). Returns a map keyed
// by service_id; a service reads `null` until two samples exist.
function useWorkloadCpu(
  workloads: ReadonlyArray<WorkloadView>,
): Map<string, number | null> {
  const [pct, setPct] = useState<Map<string, number | null>>(new Map())
  const prev = useRef<Map<string, { usec: number; at: number }>>(new Map())

  useEffect(() => {
    const now = Date.now()
    const next = new Map<string, number | null>()
    const nextPrev = new Map<string, { usec: number; at: number }>()
    for (const w of workloads) {
      if (w.cpu_usage_usec === null) {
        next.set(w.service_id, null)
        continue
      }
      const last = prev.current.get(w.service_id)
      if (last && now > last.at) {
        const dUsec = Math.max(0, w.cpu_usage_usec - last.usec)
        const elapsedUsec = (now - last.at) * 1000
        next.set(
          w.service_id,
          elapsedUsec > 0
            ? Math.max(0, Math.min(100, (dUsec / elapsedUsec) * 100))
            : null,
        )
      } else {
        next.set(w.service_id, null)
      }
      nextPrev.set(w.service_id, { usec: w.cpu_usage_usec, at: now })
    }
    prev.current = nextPrev
    setPct(next)
    // Re-derive whenever the workload counters change (each poll).
  }, [workloads])

  return pct
}

export function ObservabilityRoute() {
  const {
    data: nodeMetrics,
    isLoading: nodeLoading,
    error: nodeError,
    refetch: refetchNode,
  } = useQuery({
    queryKey: ['observability', 'node'],
    queryFn: () => runQuery(getNodeMetrics),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const {
    data: workloads = [],
    isLoading: workloadsLoading,
    error: workloadsError,
    refetch: refetchWorkloads,
  } = useQuery({
    queryKey: ['observability', 'workloads'],
    queryFn: () => runQuery(listWorkloads),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const { cpuPct, cpuSeries } = useNodeHistory(nodeMetrics)
  const workloadCpu = useWorkloadCpu(workloads)

  const running = workloads.filter((w) => w.status && w.status !== 'Stopped')

  const memPct = nodeMetrics
    ? ((nodeMetrics.memory_total_bytes - nodeMetrics.memory_available_bytes) /
        Math.max(1, nodeMetrics.memory_total_bytes)) *
      100
    : 0
  const diskPct = nodeMetrics
    ? ((nodeMetrics.disk_total_bytes - nodeMetrics.disk_available_bytes) /
        Math.max(1, nodeMetrics.disk_total_bytes)) *
      100
    : 0

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">observability</p>
          <h1 className="t-display">Node &amp; workloads</h1>
        </div>
        <span className="badge">
          <span
            className={`signal ${running.length > 0 ? 'signal-steady' : 'opacity-40'}`}
            aria-hidden="true"
          />
          <Num>{running.length}</Num> running
        </span>
      </header>

      <div className="stack-lg">
        {/* Node health: gauges + load */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            node health
          </p>
          {nodeError ? (
            <ErrorPanel
              title="Failed to load node metrics"
              message={errorMessage(nodeError)}
              onRetry={() => refetchNode()}
            />
          ) : nodeMetrics ? (
            <div className="panel panel-pad">
              <div className="flex flex-wrap items-center gap-x-10 gap-y-6">
                <div className="flex flex-col items-center gap-2">
                  <RadialGauge
                    value={cpuPct}
                    label={formatPercent(cpuPct, 0)}
                    sublabel="cpu"
                    state={usageState(cpuPct)}
                  />
                  <Sparkline
                    values={cpuSeries.length > 1 ? cpuSeries : [0, 0]}
                    width={132}
                    height={26}
                    ariaLabel="cpu trend"
                  />
                </div>
                <RadialGauge
                  value={memPct}
                  label={formatPercent(memPct, 0)}
                  sublabel="memory"
                  state={usageState(memPct)}
                />
                <RadialGauge
                  value={diskPct}
                  label={formatPercent(diskPct, 0)}
                  sublabel="disk"
                  state={usageState(diskPct)}
                />
                <dl className="flex flex-col gap-3" style={{ margin: 0 }}>
                  <Detail
                    label="memory used"
                    value={`${formatBytes(nodeMetrics.memory_total_bytes - nodeMetrics.memory_available_bytes)} / ${formatBytes(nodeMetrics.memory_total_bytes)}`}
                  />
                  <Detail
                    label="disk used"
                    value={`${formatBytes(nodeMetrics.disk_total_bytes - nodeMetrics.disk_available_bytes)} / ${formatBytes(nodeMetrics.disk_total_bytes)}`}
                  />
                  <Detail
                    label="load (1 / 5 / 15m)"
                    value={`${nodeMetrics.load_1m.toFixed(2)} ${nodeMetrics.load_5m.toFixed(2)} ${nodeMetrics.load_15m.toFixed(2)}`}
                  />
                  <Detail label="sampled" value={formatClock(nodeMetrics.recorded_at)} />
                </dl>
              </div>
            </div>
          ) : nodeLoading ? (
            <SkeletonRows rows={3} />
          ) : (
            <div className="panel">
              <EmptyState
                icon={<Activity size={22} />}
                title="Node metrics unavailable"
                hint="The control plane has not reported a node snapshot yet."
              />
            </div>
          )}
        </section>

        {/* Workloads */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            workloads
          </p>
          {workloadsError ? (
            <ErrorPanel
              title="Failed to load workloads"
              message={errorMessage(workloadsError)}
              onRetry={() => refetchWorkloads()}
            />
          ) : workloadsLoading ? (
            <SkeletonRows rows={4} />
          ) : running.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Boxes size={22} />}
                title="No running workloads"
                hint="Deploy a service to see it report live runtime state here."
                action={
                  <Link to="/services" className="btn btn-primary">
                    Go to services
                  </Link>
                }
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>service</th>
                    <th>status</th>
                    <th className="num">replicas</th>
                    <th className="num">cpu</th>
                    <th className="num">memory</th>
                    <th>usage</th>
                  </tr>
                </thead>
                <tbody>
                  {running.map((w, i) => {
                    const mem = w.memory_current_bytes ?? 0
                    const memOfNode = nodeMetrics
                      ? (mem / Math.max(1, nodeMetrics.memory_total_bytes)) * 100
                      : 0
                    const cpu = workloadCpu.get(w.service_id)
                    return (
                      <tr key={`${w.service_id}-${w.deployment_id ?? i}`}>
                        <td>
                          <Link
                            to="/services/$serviceId"
                            params={{ serviceId: w.service_id }}
                          >
                            {w.service_name}
                          </Link>
                        </td>
                        <td>{w.status ? <StatusBadge status={w.status} /> : '—'}</td>
                        <td className="num">
                          <Num
                            title={`${w.healthy_replicas} healthy of ${w.replica_count}`}
                          >
                            {w.healthy_replicas}/{w.replica_count}
                          </Num>
                        </td>
                        <td className="num">
                          <Num>
                            {cpu === null || cpu === undefined
                              ? '—'
                              : formatPercent(cpu, 1)}
                          </Num>
                        </td>
                        <td className="num">
                          <Num>
                            {w.memory_current_bytes !== null ? formatBytes(mem) : '—'}
                          </Num>
                        </td>
                        <td style={{ minWidth: '8rem' }}>
                          <BarMeter
                            value={memOfNode}
                            max={100}
                            state={usageState(memOfNode)}
                          />
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </main>
  )
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-3">
      <dt className="kicker" style={{ minWidth: '11ch' }}>
        {label}
      </dt>
      <dd className="tnum" style={{ margin: 0 }}>
        {value}
      </dd>
    </div>
  )
}
