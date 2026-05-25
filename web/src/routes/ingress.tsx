import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'

const listRoutes = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listRoutes
})

const getConfig = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getIngressConfig
})

export const Route = createFileRoute('/ingress')({
  component: IngressRoute,
})

export function IngressRoute() {
  const [showConfig, setShowConfig] = useState(false)
  const [copied, setCopied] = useState(false)

  const { data: routes = [], isFetching } = useQuery({
    queryKey: ['ingress', 'routes'],
    queryFn: () => runQuery(listRoutes),
  })

  const { data: config = '', isFetching: configFetching, refetch: refetchConfig } = useQuery({
    queryKey: ['ingress', 'config'],
    queryFn: () => runQuery(getConfig),
    enabled: false,
  })

  const handleToggleConfig = () => {
    if (!showConfig && !config) {
      refetchConfig()
    }
    setShowConfig(!showConfig)
  }

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(config)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      // clipboard API not available
    }
  }

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
                <th className="px-4 py-2 font-semibold tnum">port</th>
                <th className="px-4 py-2 font-semibold">transport</th>
              </tr>
            </thead>
            <tbody>
              {routes.map((r, i) => (
                <tr
                  key={`${r.service_name}-${r.bridge_port}`}
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
                  <td className="px-4 py-3 tnum text-xs text-[var(--fg-muted)]">
                    {r.bridge_port}
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

      <section className="mt-8">
        <button
          className="btn text-xs"
          type="button"
          onClick={handleToggleConfig}
        >
          {showConfig ? 'hide' : 'raw config'}
        </button>

        {showConfig && (
          <div className="mt-3">
            {configFetching ? (
              <p className="text-sm text-[var(--fg-muted)]">
                fetching config...
              </p>
            ) : config ? (
              <div className="panel overflow-hidden">
                <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
                  <p className="kicker m-0 flex-1">traefik yaml</p>
                  <button
                    className="btn text-xs"
                    type="button"
                    onClick={handleCopy}
                  >
                    {copied ? 'copied' : 'copy'}
                  </button>
                </div>
                <pre className="m-0 overflow-x-auto p-4">
                  <code className="block whitespace-pre font-mono text-xs text-[var(--fg)]">
                    {config}
                  </code>
                </pre>
              </div>
            ) : (
              <p className="text-sm text-[var(--fg-muted)]">
                Could not load config.
              </p>
            )}
          </div>
        )}
      </section>
    </main>
  )
}
