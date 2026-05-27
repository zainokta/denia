import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'

const listRoutes = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listRoutes
})

export const Route = createFileRoute('/ingress')({
  component: IngressRoute,
})

export function IngressRoute() {
  const { data: routes = [], isFetching } = useQuery({
    queryKey: ['ingress', 'routes'],
    queryFn: () => runQuery(listRoutes),
  })

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">ingress</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Routes
      </h1>

      {routes.length === 0 && !isFetching ? (
        <p className="text-[var(--fg-muted)]">
          No routes yet. Deploy a service to generate ingress routes.
        </p>
      ) : (
        <section className="panel overflow-hidden">
          <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
            <p className="kicker m-0">
              {isFetching
                ? 'fetching...'
                : `${routes.length} route${routes.length !== 1 ? 's' : ''}`}
            </p>
          </div>
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-[var(--border)] text-xs text-[var(--fg-muted)]">
                <th className="px-4 py-2 font-semibold">domain(s)</th>
                <th className="px-4 py-2 font-semibold">service</th>
                <th className="px-4 py-2 font-semibold">transport</th>
              </tr>
            </thead>
            <tbody>
              {routes.map((r, i) => (
                <tr
                  key={`${r.service_name}-${r.domains.join(',')}`}
                  className={
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }
                >
                  <td className="px-4 py-3 text-xs text-[var(--fg)]">
                    {r.domains.join(', ')}
                  </td>
                  <td className="px-4 py-3 text-xs text-[var(--fg)]">
                    {r.service_name}
                  </td>
                  <td className="px-4 py-3">
                    {r.tls ? (
                      <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
                        <span
                          className="signal signal-steady"
                          aria-hidden="true"
                        />
                        TLS
                      </span>
                    ) : (
                      <span className="text-xs text-[var(--fg-muted)]">
                        http
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </main>
  )
}
