import { createFileRoute, Link, useParams } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ArrowLeft } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { DeployPhase } from '#/components/DeployPhase'
import { StatusBadge } from '#/components/StatusBadge'
import { LogStream } from '#/components/LogStream'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { formatDateTime, formatRelative, shortId } from '#/lib/format'

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

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <header style={{ marginBottom: '1.5rem' }}>
        {deployment ? (
          <Link
            to="/services/$serviceId"
            params={{ serviceId: deployment.service_id }}
            className="cluster"
            style={{ gap: '0.4rem', fontSize: 'var(--text-label)', color: 'var(--fg-muted)' }}
          >
            <ArrowLeft size={13} aria-hidden="true" /> back to service
          </Link>
        ) : (
          <p className="kicker">deployment</p>
        )}
        <div className="panel-head" style={{ marginTop: '0.5rem' }}>
          <h1 className="t-display tnum">{shortId(deployment ? deployment.id : id)}</h1>
          {deployment ? <StatusBadge status={deployment.status} /> : null}
        </div>
        {deployment ? (
          <div style={{ marginTop: '0.75rem' }}>
            <DeployPhase status={deployment.status} />
          </div>
        ) : null}
      </header>

      {error ? (
        <ErrorPanel
          title="Failed to load deployment"
          message={errorMessage(error)}
        />
      ) : null}

      <div className="stack">
        {deployment ? (
          <section className="panel panel-pad">
            <dl className="flex flex-col gap-3" style={{ margin: 0 }}>
              <Row label="service">
                <Link to="/services/$serviceId" params={{ serviceId: deployment.service_id }}>
                  {shortId(deployment.service_id)}
                </Link>
              </Row>
              <Row label="created">
                <span className="tnum" title={formatDateTime(deployment.created_at)}>
                  {formatRelative(deployment.created_at, Date.now())}
                </span>
              </Row>
              <Row label="artifact">
                {deployment.artifact ? (
                  <span className="cluster" style={{ gap: '0.5rem' }}>
                    <code className="tnum">{deployment.artifact.digest.slice(0, 16)}</code>
                    <span className="badge">
                      {deployment.artifact.kind === 'OciImage' ? 'image' : 'bundle'}
                    </span>
                  </span>
                ) : (
                  <span className="text-faint">pending</span>
                )}
              </Row>
            </dl>
          </section>
        ) : null}

        <section>
          <p className="kicker" style={{ marginBottom: '0.6rem' }}>
            deploy log
          </p>
          <LogStream
            path={`/v1/deployments/${id}/logs`}
            title="deploy"
            showLineNumbers
            height="32rem"
          />
        </section>
      </div>
    </main>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline gap-3">
      <dt className="kicker" style={{ minWidth: '7rem' }}>
        {label}
      </dt>
      <dd style={{ margin: 0 }}>{children}</dd>
    </div>
  )
}
