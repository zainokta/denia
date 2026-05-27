import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useMemo } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'
import { DeployPhase } from '#/components/DeployPhase'

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

function CpuPercent(cpu: {
  user_jiffies: number
  nice_jiffies: number
  system_jiffies: number
  idle_jiffies: number
  iowait_jiffies: number
}): string {
  const total =
    cpu.user_jiffies +
    cpu.nice_jiffies +
    cpu.system_jiffies +
    cpu.idle_jiffies +
    cpu.iowait_jiffies
  if (total === 0) return '0.0'
  const busy =
    cpu.user_jiffies + cpu.nice_jiffies + cpu.system_jiffies + cpu.iowait_jiffies
  return ((busy / total) * 100).toFixed(1)
}

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824)
    return `${(bytes / 1_073_741_824).toFixed(1)} GiB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MiB`
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${bytes} B`
}

function formatDisk(total: number, available: number): string {
  const used = total - available
  const pct = total > 0 ? ((used / total) * 100).toFixed(1) : '0.0'
  return `${formatBytes(used)} / ${formatBytes(total)} (${pct}%)`
}

export function Dashboard() {
  const { data: nodeMetrics } = useQuery({
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

  const { data: services = [] } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const running = workloads.filter(
    (w) => w.status && w.status !== 'Stopped',
  )

  const hasServices = services.length > 0

  const serviceIds = useMemo(
    () => services.map((s) => s.id),
    [services],
  )

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      {/* Node health summary — dense, mono, flat */}
      {nodeMetrics ? (
        <section className="mb-10">
          <p className="kicker mb-3">node health</p>
          <div className="flex flex-wrap gap-x-8 gap-y-2 text-sm">
            <span className="tnum inline-flex items-baseline gap-2 text-[var(--fg)]">
              <span className="text-xs text-[var(--fg-muted)]">cpu</span>
              {CpuPercent(nodeMetrics.cpu)}%
            </span>
            <span className="tnum inline-flex items-baseline gap-2 text-[var(--fg)]">
              <span className="text-xs text-[var(--fg-muted)]">mem</span>
              {formatBytes(
                nodeMetrics.memory_total_bytes -
                  nodeMetrics.memory_available_bytes,
              )}{' '}
              / {formatBytes(nodeMetrics.memory_total_bytes)}
            </span>
            <span className="tnum inline-flex items-baseline gap-2 text-[var(--fg)]">
              <span className="text-xs text-[var(--fg-muted)]">disk</span>
              {formatDisk(
                nodeMetrics.disk_total_bytes,
                nodeMetrics.disk_available_bytes,
              )}
            </span>
            <span className="tnum inline-flex items-baseline gap-2 text-[var(--fg)]">
              <span className="text-xs text-[var(--fg-muted)]">load</span>
              {nodeMetrics.load_1m.toFixed(2)} /{' '}
              {nodeMetrics.load_5m.toFixed(2)} /{' '}
              {nodeMetrics.load_15m.toFixed(2)}
            </span>
          </div>
        </section>
      ) : null}

      {/* Running workloads */}
      <section className="mb-10">
        <p className="kicker mb-3">
          workloads{' '}
          <span className="text-[var(--fg-muted)]">
            {running.length} running
          </span>
        </p>
        {running.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">
            No running workloads.
          </p>
        ) : (
          <div className="panel overflow-hidden">
            <ul className="m-0 list-none">
              {running.slice(0, 5).map((w, i) => (
                <li
                  key={`${w.service_id}-${w.deployment_id ?? i}`}
                  className={`flex items-center gap-4 px-4 py-2.5 text-sm ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  {w.status ? <StatusSignal status={w.status} /> : null}
                  <span className="font-semibold text-[var(--fg)] min-w-0 truncate">
                    {w.service_name}
                  </span>
                  {w.cpu_usage_usec !== null ? (
                    <span className="tnum text-xs text-[var(--fg-muted)] ml-auto">
                      {(w.cpu_usage_usec / 10000).toFixed(1)}%
                    </span>
                  ) : null}
                  {w.memory_current_bytes !== null ? (
                    <span className="tnum text-xs text-[var(--fg-muted)]">
                      {formatBytes(w.memory_current_bytes)}
                    </span>
                  ) : null}
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      {/* Recent deployments timeline — last 5 across all services */}
      <section className="mb-10">
        <p className="kicker mb-3">recent deployments</p>
        {serviceIds.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">
            No services yet.
          </p>
        ) : (
          <RecentDeployments serviceIds={serviceIds} />
        )}
      </section>

      {/* Getting started when no services exist */}
      {!hasServices ? (
        <section className="panel p-6">
          <p className="kicker mb-3">getting started</p>
          <p className="mb-4 text-sm text-[var(--fg-muted)]">
            Create a project, then deploy your first service.
          </p>
          <div className="flex flex-wrap gap-3">
            <a href="/projects" className="btn btn-primary">
              Projects
            </a>
            <a href="/services" className="btn">
              Services
            </a>
          </div>
        </section>
      ) : null}
    </main>
  )
}

function RecentDeployments({ serviceIds }: { serviceIds: string[] }) {
  // Fetch deployments for each service — in a real app with many services
  // we'd want a backend aggregate endpoint, but for solo-operator scale
  // this is fine.
  return (
    <div className="panel overflow-hidden">
      <DeploymentRows serviceIds={serviceIds} maxRows={5} />
    </div>
  )
}

function DeploymentRows({
  serviceIds,
  maxRows,
}: {
  serviceIds: string[]
  maxRows: number
}) {
  // Fetch the first service's deployments to render something useful
  // without excessive parallel queries. In practice, a single-node operator
  // has few services. We render inline loading per service.
  return (
    <>
      {serviceIds.slice(0, maxRows).map((id) => (
        <ServiceDeploymentRow key={id} serviceId={id} />
      ))}
    </>
  )
}

function ServiceDeploymentRow({ serviceId }: { serviceId: string }) {
  const { data: deployments = [] } = useQuery({
    queryKey: ['services', serviceId, 'deployments'],
    queryFn: () => runQuery(getServiceDeployments(serviceId)),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  if (deployments.length === 0) return null

  const newest = deployments.reduce((a, b) => (a.id > b.id ? a : b))

  return (
    <div className="flex items-center gap-4 px-4 py-2.5 text-sm border-t border-[var(--border)] first:border-t-0">
      <StatusSignal status={newest.status} />
      <DeployPhase status={newest.status} />
      <span className="tnum text-xs text-[var(--fg-muted)] ml-auto">
        {newest.created_at}
      </span>
    </div>
  )
}
