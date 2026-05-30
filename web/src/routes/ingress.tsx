import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { Globe } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { Num } from '#/components/Num'

const listRoutes = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listRoutes
})

export const Route = createFileRoute('/ingress')({
  component: IngressRoute,
})

export function IngressRoute() {
  const {
    data: routes = [],
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['ingress', 'routes'],
    queryFn: () => runQuery(listRoutes),
  })

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">ingress</p>
          <h1 className="t-display">Routes</h1>
        </div>
        {routes.length > 0 ? (
          <span className="badge">
            <Num>{routes.length}</Num> route{routes.length !== 1 ? 's' : ''}
          </span>
        ) : null}
      </header>

      <div className="stack-lg">
        <section>
          {error ? (
            <ErrorPanel
              title="Failed to load routes"
              message={errorMessage(error)}
              onRetry={() => refetch()}
            />
          ) : isLoading ? (
            <SkeletonRows rows={4} />
          ) : routes.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Globe size={22} />}
                title="No routes yet"
                hint="Verify a domain on a service to publish an ingress route here."
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>domain(s)</th>
                    <th>service</th>
                    <th>transport</th>
                  </tr>
                </thead>
                <tbody>
                  {routes.map((r) => (
                    <tr key={`${r.service_name}-${r.domains.join(',')}`}>
                      <td>
                        <div className="flex flex-col gap-1">
                          {r.domains.length > 0 ? (
                            r.domains.map((domain) => (
                              <span key={domain}>{domain}</span>
                            ))
                          ) : (
                            <span className="text-faint">—</span>
                          )}
                        </div>
                      </td>
                      <td>{r.service_name}</td>
                      <td>
                        {r.tls ? (
                          <span className="badge badge-ok">TLS</span>
                        ) : (
                          <span className="badge">http</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </main>
  )
}
