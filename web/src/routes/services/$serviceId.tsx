import { createFileRoute, useParams } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'

const getDeployments = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceDeployments(id)
  })

const getLogs = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceLogs(id)
  })

const getMetrics = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceMetrics(id)
  })

const createDeployment = (serviceId: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createDeployment({ service_id: serviceId })
  })

const stopService = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.stopService(id)
  })

export const Route = createFileRoute('/services/$serviceId')({
  component: ServiceDetail,
})

export function ServiceDetail() {
  const params = useParams({ from: '/services/$serviceId' })
  const id = Number(params.serviceId)
  const queryClient = useQueryClient()

  const {
    data: deployments = [],
    isFetching: deploymentsFetching,
  } = useQuery({
    queryKey: ['services', id, 'deployments'],
    queryFn: () => runQuery(getDeployments(id)),
  })

  const { data: logs = [] } = useQuery({
    queryKey: ['services', id, 'logs'],
    queryFn: () => runQuery(getLogs(id)),
    refetchInterval: 3000,
    refetchIntervalInBackground: false,
  })

  const { data: metrics = [] } = useQuery({
    queryKey: ['services', id, 'metrics'],
    queryFn: () => runQuery(getMetrics(id)),
    refetchInterval: 3000,
    refetchIntervalInBackground: false,
  })

  const deploy = useMutation({
    mutationFn: () => runQuery(createDeployment(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'deployments'],
      })
    },
  })

  const stop = useMutation({
    mutationFn: () => runQuery(stopService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'deployments'],
      })
    },
  })

  const newestFirst = [...deployments].reverse()

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">
        service{' '}
        <a href="/services" className="text-[var(--fg-muted)]">
          &larr; back
        </a>
      </p>
      <div className="mb-6 flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold tracking-tight text-[var(--fg)]">
          #{id}
        </h1>
        <button
          className="btn btn-primary text-xs"
          type="button"
          onClick={() => deploy.mutate()}
          disabled={deploy.isPending}
        >
          {deploy.isPending ? 'deploying...' : 'deploy'}
        </button>
        <button
          className="btn text-xs"
          type="button"
          onClick={() => stop.mutate()}
          disabled={stop.isPending}
        >
          stop
        </button>
      </div>

      <section className="mb-8">
        <p className="kicker mb-2">
          deployments{' '}
          {deploymentsFetching ? (
            <span className="text-[var(--fg-muted)]">fetching...</span>
          ) : (
            <span className="text-[var(--fg-muted)]">
              {deployments.length}
            </span>
          )}
        </p>
        {newestFirst.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">
            No deployments yet.
          </p>
        ) : (
          <div className="panel overflow-hidden">
            <ul className="m-0 list-none">
              {newestFirst.map((d, i) => (
                <li
                  key={d.id}
                  className={`flex items-center gap-4 px-4 py-3 text-sm ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  <StatusSignal status={d.status} />
                  <span className="tnum text-xs text-[var(--fg-muted)]">
                    {d.created_at}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      <section className="mb-8">
        <p className="kicker mb-2">logs</p>
        {logs.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">
            No logs available.
          </p>
        ) : (
          <div className="panel overflow-hidden">
            <ul className="m-0 list-none">
              {logs.map((line, i) => (
                <li
                  key={i}
                  className={`flex gap-4 px-4 py-1.5 text-xs ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  <span className="tnum flex-shrink-0 text-[var(--fg-muted)]">
                    {String(i + 1).padStart(3, '0')}
                  </span>
                  <code className="flex-1 whitespace-pre-wrap break-all font-mono text-[var(--fg)]">
                    {line}
                  </code>
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      <section>
        <p className="kicker mb-2">metrics</p>
        {metrics.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">
            No metrics available.
          </p>
        ) : (
          <div className="panel overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="border-b border-[var(--border)] text-xs text-[var(--fg-muted)]">
                  <th className="px-4 py-2 font-semibold">timestamp</th>
                  <th className="px-4 py-2 font-semibold tnum">cpu %</th>
                  <th className="px-4 py-2 font-semibold tnum">memory</th>
                </tr>
              </thead>
              <tbody>
                {metrics.map((m, i) => (
                  <tr
                    key={i}
                    className={
                      i > 0 ? 'border-t border-[var(--border)]' : ''
                    }
                  >
                    <td className="px-4 py-2 text-xs text-[var(--fg-muted)]">
                      {m.recorded_at}
                    </td>
                    <td className="px-4 py-2 tnum text-xs text-[var(--fg)]">
                      {(m.cpu_percent * 100).toFixed(1)}%
                    </td>
                    <td className="px-4 py-2 tnum text-xs text-[var(--fg)]">
                      {formatBytes(m.memory_bytes)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </main>
  )
}

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GiB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MiB`
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${bytes} B`
}
