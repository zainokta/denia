import { createFileRoute, Link, useParams } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { DeployPhase } from '#/components/DeployPhase'
import { useDeploymentLogs } from '#/hooks/useDeploymentLogs'

const getDeployment = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getDeployment(id)
  })

function isActive(status: string): boolean {
  return status === 'Pending' || status === 'Building' || status === 'Starting'
}

export const Route = createFileRoute('/deployments/$deploymentId')({
  component: DeploymentDetail,
})

export function DeploymentDetail() {
  const params = useParams({ from: '/deployments/$deploymentId' })
  const id = params.deploymentId

  const { data: deployment, error } = useQuery({
    queryKey: ['deployments', id],
    queryFn: () => runQuery(getDeployment(id)),
    refetchInterval: (query) => {
      const data = query.state.data
      if (data && isActive(data.status)) return 2000
      return false
    },
    refetchIntervalInBackground: false,
  })

  const { lines, error: logsError, done } = useDeploymentLogs(id, true)

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">
        deployment{' '}
        {deployment ? (
          <Link
            to="/services/$serviceId"
            params={{ serviceId: deployment.service_id }}
            className="text-[var(--fg-muted)]"
          >
            &larr; back to service
          </Link>
        ) : (
          <span className="text-[var(--fg-muted)]">&larr; back</span>
        )}
      </p>

      <div className="mb-6 flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold tracking-tight text-[var(--fg)]">
          {deployment ? deployment.id : id}
        </h1>
        {deployment ? (
          <>
            <span className="kicker">{deployment.status}</span>
            <DeployPhase status={deployment.status} />
          </>
        ) : null}
      </div>

      {error ? (
        <div className="panel mb-4 p-3 text-xs text-[var(--fg)]">
          <span className="signal signal-fault mr-2 inline-block align-middle" />
          {error instanceof Error ? error.message : 'Failed to load deployment'}
        </div>
      ) : null}

      {deployment ? (
        <div className="panel mb-6 overflow-hidden">
          <ul className="m-0 list-none">
            <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
              <span className="kicker w-32">service</span>
              <Link
                to="/services/$serviceId"
                params={{ serviceId: deployment.service_id }}
                className="tnum text-[var(--fg)]"
              >
                {deployment.service_id}
              </Link>
            </li>
            <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
              <span className="kicker w-32">created</span>
              <span className="tnum text-[var(--fg-muted)]">
                {deployment.created_at}
              </span>
            </li>
            <li className="flex items-center gap-3 px-4 py-2.5 text-sm">
              <span className="kicker w-32">artifact</span>
              {deployment.artifact ? (
                <span className="flex flex-wrap items-center gap-2 text-xs">
                  <code className="tnum text-[var(--fg)]">
                    {deployment.artifact.digest.slice(0, 12)}
                  </code>
                  <span className="text-[var(--fg-muted)]">
                    {deployment.artifact.kind === 'OciImage'
                      ? 'image'
                      : 'bundle'}
                  </span>
                </span>
              ) : (
                <span className="text-xs text-[var(--fg-muted)]">pending</span>
              )}
            </li>
          </ul>
        </div>
      ) : null}

      <div className="mb-2 flex items-center gap-2 text-xs">
        {logsError ? (
          <span className="text-[var(--violet)]">
            <span className="signal signal-fault mr-2 inline-block align-middle" />
            {logsError}
          </span>
        ) : done ? (
          <>
            <span className="signal" />
            <span className="kicker">closed</span>
            <span className="tnum text-[var(--fg-muted)]">
              {lines.length} line{lines.length === 1 ? '' : 's'}
            </span>
          </>
        ) : (
          <>
            <span className="signal signal-steady" />
            <span className="kicker">live</span>
            <span className="tnum text-[var(--fg-muted)]">
              {lines.length} line{lines.length === 1 ? '' : 's'}
            </span>
          </>
        )}
      </div>

      {lines.length === 0 ? (
        <p className="text-sm text-[var(--fg-muted)]">
          {logsError ? 'Stream unavailable.' : 'Waiting for logs...'}
        </p>
      ) : (
        <div className="panel overflow-hidden">
          <ul className="m-0 list-none">
            {lines.map((line, i) => (
              <li
                key={`${i}:${line}`}
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
          {done ? (
            <p className="px-4 py-2 text-xs text-[var(--fg-muted)]">
              stream closed
            </p>
          ) : null}
        </div>
      )}
    </main>
  )
}
