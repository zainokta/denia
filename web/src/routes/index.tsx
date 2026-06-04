import { createFileRoute, Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Boxes, Rocket } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { NodeSnapshot } from '#/effect/schema'
import { BarMeter, RadialGauge, Sparkline } from '#/components/Charts'
import { StatusBadge } from '#/components/StatusBadge'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { Num } from '#/components/Num'
import type { SemState } from '#/lib/status'
import { cpuBusyTotal, cpuPercentDelta } from '#/lib/metrics'
import { formatBytes, formatPercent, formatRelative, shortId } from '#/lib/format'
import { useActiveProject } from '#/hooks/useActiveProject'

const getNodeMetrics = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getNodeMetrics
})

const listWorkloads = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listWorkloads
})

const listServices = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listServices
})

const getServiceDeployments = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceDeployments(id)
  })

export const Route = createFileRoute('/')({ component: Dashboard })

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

export function Dashboard() {
  const [activeProject] = useActiveProject()

  const { data: nodeMetrics, isLoading: nodeLoading } = useQuery({
    queryKey: ['node', 'metrics'],
    queryFn: () => runQuery(getNodeMetrics),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const { data: workloads = [] } = useQuery({
    queryKey: ['workloads'],
    queryFn: () => runQuery(listWorkloads),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const { data: allServices = [], isLoading: servicesLoading } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const { cpuPct, cpuSeries } = useNodeHistory(nodeMetrics)

  // Active-project scoping: when a project is selected in the switcher, the
  // workloads and recent-deployment lists narrow to that project. Empty = all.
  const services = useMemo(
    () =>
      activeProject
        ? allServices.filter((s) => s.project_id === activeProject)
        : allServices,
    [allServices, activeProject],
  )
  const scopedWorkloads = useMemo(
    () =>
      activeProject
        ? workloads.filter((w) => w.project_id === activeProject)
        : workloads,
    [workloads, activeProject],
  )

  const running = scopedWorkloads.filter((w) => w.status && w.status !== 'Stopped')
  const hasServices = services.length > 0
  const serviceIds = useMemo(() => services.map((s) => s.id), [services])

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
    <div className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">control plane</p>
          <h1 className="t-display">Overview</h1>
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
          {nodeMetrics ? (
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
                </dl>
              </div>
            </div>
          ) : nodeLoading ? (
            <SkeletonRows rows={3} />
          ) : (
            <div className="panel panel-pad">
              <p className="text-faint">Node metrics unavailable.</p>
            </div>
          )}
        </section>

        {/* Running workloads */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            workloads
          </p>
          {running.length === 0 ? (
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
                    <th className="num">memory</th>
                    <th>usage</th>
                  </tr>
                </thead>
                <tbody>
                  {running.slice(0, 8).map((w, i) => {
                    const mem = w.memory_current_bytes ?? 0
                    const memOfNode = nodeMetrics
                      ? (mem / Math.max(1, nodeMetrics.memory_total_bytes)) * 100
                      : 0
                    return (
                      <tr key={`${w.service_id}-${w.deployment_id ?? i}`}>
                        <td>
                          <Link to="/services/$serviceId" params={{ serviceId: w.service_id }}>
                            {w.service_name}
                          </Link>
                        </td>
                        <td>
                          {w.status ? <StatusBadge status={w.status} /> : '—'}
                        </td>
                        <td className="num">
                          <Num>{w.memory_current_bytes !== null ? formatBytes(mem) : '—'}</Num>
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

        {/* Recent deployments */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            recent deployments
          </p>
          {servicesLoading ? (
            <SkeletonRows rows={3} />
          ) : serviceIds.length === 0 ? (
            <div className="panel">
              <EmptyState title="No deployments yet" hint="They will appear here once you deploy." />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              {serviceIds.slice(0, 6).map((id) => (
                <ServiceDeploymentRow key={id} serviceId={id} />
              ))}
            </div>
          )}
        </section>

        {/* Getting started */}
        {!servicesLoading && !hasServices ? (
          <section className="panel panel-pad">
            <p className="kicker" style={{ marginBottom: '0.6rem' }}>
              getting started
            </p>
            <p className="text-faint" style={{ marginBottom: '1rem', maxWidth: '52ch' }}>
              Create a project to scope your work, then define a service and deploy it.
              Routes, TLS, and runtime metrics follow automatically.
            </p>
            <div className="cluster">
              <Link to="/projects" className="btn btn-primary">
                <Rocket size={14} aria-hidden="true" /> Create a project
              </Link>
              <Link to="/services" className="btn">
                Services
              </Link>
            </div>
          </section>
        ) : null}
      </div>
    </div>
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

function ServiceDeploymentRow({ serviceId }: { serviceId: string }) {
  // One-shot fetch per service (capped at 6 rows). The newest in-progress
  // deployment self-polls on its own detail page; the landing page does not run
  // N parallel polling loops — it refreshes when the user returns to the tab.
  const { data: deployments = [] } = useQuery({
    queryKey: ['services', serviceId, 'deployments'],
    queryFn: () => runQuery(getServiceDeployments(serviceId)),
    staleTime: 15000,
  })

  if (deployments.length === 0) return null
  const newest = deployments.reduce((a, b) => (a.id > b.id ? a : b))

  return (
    <Link
      to="/deployments/$deploymentId"
      params={{ deploymentId: newest.id }}
      className="flex items-center gap-4 px-4 py-3 border-t border-[var(--border)] first:border-t-0 no-underline hover:no-underline"
      style={{ color: 'inherit' }}
    >
      <StatusBadge status={newest.status} />
      <span className="kicker" style={{ color: 'var(--fg-faint)' }}>
        {shortId(newest.id)}
      </span>
      <span className="tnum text-faint ml-auto" style={{ fontSize: 'var(--text-label)' }}>
        {formatRelative(newest.created_at, Date.now())}
      </span>
    </Link>
  )
}
