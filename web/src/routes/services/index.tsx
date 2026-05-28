import { createFileRoute } from '@tanstack/react-router'
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { useState } from 'react'
import { ChevronDown, ChevronUp } from 'lucide-react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'
import { ServiceForm } from '#/components/ServiceForm'
import { useAuth, can } from '#/hooks/useAuth'
import type {
  Service,
  ServiceInput,
  WorkloadView,
} from '#/effect/schema'

export const listServices = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listServices
})

export const listWorkloads = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listWorkloads
})

export const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

const listRegistries = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listRegistries(projectId)
  })

export const createService = (input: ServiceInput | Service) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.putService(input)
  })

export const deleteService = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteService(id)
  })

const createDeployment = (service: Service) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createDeployment(service)
  })

const stopService = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.stopService(id)
  })

export const Route = createFileRoute('/services/')({
  component: ServicesIndex,
})

type DeploymentStatusValue = NonNullable<WorkloadView['status']>

export function ServicesIndex() {
  const queryClient = useQueryClient()
  const { isSuperAdmin, roleForActiveProject } = useAuth()

  const [createOpen, setCreateOpen] = useState(false)
  const [createError, setCreateError] = useState('')
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState('')

  const { data: services = [], isFetching } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const { data: workloads = [] } = useQuery({
    queryKey: ['workloads'],
    queryFn: () => runQuery(listWorkloads),
  })

  const { data: projects = [] } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  const manageableProjects = projects.filter(
    (p) => isSuperAdmin || roleForActiveProject(p.id) === 'admin',
  )

  const registryQueries = useQueries({
    queries: manageableProjects.map((p) => ({
      queryKey: ['projects', p.id, 'registries'],
      queryFn: () => runQuery(listRegistries(p.id)),
    })),
  })

  const registries = registryQueries.flatMap((q) => q.data ?? [])

  const statusByService = new Map<string, DeploymentStatusValue | null>()
  for (const wl of workloads) {
    statusByService.set(wl.service_id, wl.status)
  }

  const canOperate = (projectId: string): boolean => {
    if (isSuperAdmin) return true
    const role = roleForActiveProject(projectId)
    return role !== undefined && can('operator', role)
  }

  const canOperateService = (): boolean => {
    if (isSuperAdmin) return true
    for (const svc of services) {
      if (canOperate(svc.project_id)) return true
    }
    return false
  }

  const create = useMutation({
    mutationFn: (input: ServiceInput | Service) =>
      runQuery(createService(input)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      setCreateOpen(false)
      setCreateError('')
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : 'Create failed'
      setCreateError(msg)
    },
  })

  const remove = useMutation({
    mutationFn: (id: string) => runQuery(deleteService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      setDeleteConfirm(null)
      setDeleteError('')
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : 'Delete failed'
      setDeleteError(msg)
    },
  })

  const deploy = useMutation({
    mutationFn: (service: Service) =>
      runQuery(createDeployment(service)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
    },
  })

  const stop = useMutation({
    mutationFn: (id: string) => runQuery(stopService(id)),
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

      {canOperateService() ? (
        <section className="panel mb-6 overflow-hidden">
          <button
            type="button"
            className="flex w-full items-center justify-between px-4 py-2.5 text-left text-sm font-semibold text-[var(--fg)] hover:bg-[var(--surface-2)]"
            aria-expanded={createOpen}
            aria-controls="new-service-panel"
            onClick={() => setCreateOpen((v) => !v)}
          >
            <span className="flex items-center gap-2">
              <span className="signal signal-steady" aria-hidden="true" />
              new service
            </span>
            {createOpen ? (
              <ChevronUp size={14} className="text-[var(--fg-muted)]" aria-hidden="true" />
            ) : (
              <ChevronDown size={14} className="text-[var(--fg-muted)]" aria-hidden="true" />
            )}
          </button>

          {createOpen ? (
            <div
              id="new-service-panel"
              className="border-t border-[var(--border)] px-4 py-3"
            >
              <ServiceForm
                projects={projects.map((p) => ({ id: p.id, name: p.name }))}
                registries={registries.map((r) => ({
                  id: r.id,
                  name: r.name,
                  project_id: r.project_id,
                  endpoint: r.endpoint,
                }))}
                pending={create.isPending}
                error={createError || undefined}
                onSubmit={(value) => create.mutate(value)}
              />
            </div>
          ) : null}
        </section>
      ) : null}

      {services.length === 0 && !isFetching ? (
        <p className="text-[var(--fg-muted)]">
          No services yet. Create a project and deploy your first service.
        </p>
      ) : (
        <section className="panel overflow-hidden">
          <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
            <p className="kicker m-0">
              {isFetching
                ? 'fetching...'
                : `${services.length} service${services.length !== 1 ? 's' : ''}`}
            </p>
          </div>
          {deleteError ? (
            <div className="border-b border-[var(--border)] px-4 py-2 text-xs text-[var(--violet)]">
              <span className="signal signal-fault mr-2 inline-block align-middle" />
              {deleteError}
            </div>
          ) : null}
          <ul className="m-0 list-none">
            {services.map((svc, i) => {
              const status = statusByService.get(svc.id) ?? null
              return (
                <li
                  key={svc.id}
                  className={`flex items-center gap-4 px-4 py-3 text-sm ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  {status ? (
                    <StatusSignal status={status} />
                  ) : null}
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
                        onClick={() => deploy.mutate(svc)}
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
                      {deleteConfirm === svc.id ? (
                        <span className="inline-flex items-center gap-1 text-xs">
                          <span className="text-[var(--violet)]">delete?</span>
                          <button
                            type="button"
                            className="btn text-xs"
                            aria-label="confirm delete service"
                            onClick={() => remove.mutate(svc.id)}
                            disabled={remove.isPending}
                          >
                            yes
                          </button>
                          <button
                            type="button"
                            className="btn text-xs"
                            aria-label="cancel delete service"
                            onClick={() => setDeleteConfirm(null)}
                          >
                            no
                          </button>
                        </span>
                      ) : (
                        <button
                          type="button"
                          className="btn text-xs"
                          onClick={() => {
                            setDeleteError('')
                            setDeleteConfirm(svc.id)
                          }}
                        >
                          delete
                        </button>
                      )}
                    </>
                  ) : null}
                </li>
              )
            })}
          </ul>
        </section>
      )}
    </main>
  )
}
