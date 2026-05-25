import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'
import { SecurityBadge } from '#/components/SecurityBadge'
import { useAuth, can } from '#/hooks/useAuth'

const listServices = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listServices
})

const createDeployment = (input: { service_id: number }) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createDeployment(input)
  })

const stopService = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.stopService(id)
  })

export const Route = createFileRoute('/services/')({
  component: ServicesIndex,
})

export function ServicesIndex() {
  const queryClient = useQueryClient()
  const { isSuperAdmin, roleForActiveProject } = useAuth()

  const { data: services = [], isFetching } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const canOperate = (projectId: number): boolean => {
    if (isSuperAdmin) return true
    const role = roleForActiveProject(String(projectId))
    return role !== undefined && can('operator', role)
  }

  const deploy = useMutation({
    mutationFn: (id: number) =>
      runQuery(createDeployment({ service_id: id })),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
    },
  })

  const stop = useMutation({
    mutationFn: (id: number) => runQuery(stopService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
    },
  })

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">services</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Services
      </h1>

      {services.length === 0 && !isFetching ? (
        <p className="text-[var(--fg-muted)]">
          No services yet. Create a project and deploy your first service.
        </p>
      ) : (
        <section className="panel overflow-hidden">
          <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
            <p className="kicker m-0">
              {isFetching ? 'fetching...' : `${services.length} service${services.length !== 1 ? 's' : ''}`}
            </p>
          </div>
          <ul className="m-0 list-none">
            {services.map((svc, i) => (
              <li
                key={svc.id}
                className={`flex items-center gap-4 px-4 py-3 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                {svc.status ? <StatusSignal status={svc.status} /> : null}
                <SecurityBadge security={svc.security} />
                <a
                  href={`/services/${svc.id}`}
                  className="min-w-0 flex-1 text-[var(--fg)] no-underline hover:underline"
                >
                  <span className="font-semibold">{svc.name}</span>
                  <span className="ml-3 text-xs text-[var(--fg-muted)]">
                    {svc.domains.join(', ') || `:${svc.internal_port}`}
                  </span>
                </a>
                {canOperate(svc.project_id) ? (
                  <>
                    <button
                      className="btn btn-primary text-xs"
                      type="button"
                      onClick={() => deploy.mutate(svc.id)}
                      disabled={deploy.isPending}
                    >
                      {deploy.isPending ? 'deploying...' : 'deploy'}
                    </button>
                    <button
                      className="btn text-xs"
                      type="button"
                      onClick={() => stop.mutate(svc.id)}
                      disabled={stop.isPending}
                    >
                      stop
                    </button>
                  </>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      )}
    </main>
  )
}
