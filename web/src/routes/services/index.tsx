import { createFileRoute, Link } from '@tanstack/react-router'
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { useState } from 'react'
import { Boxes, ChevronDown, ChevronUp, Rocket } from 'lucide-react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { ServiceForm } from '#/components/ServiceForm'
import { StatusBadge } from '#/components/StatusBadge'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { InlineError, errorMessage } from '#/components/ErrorPanel'
import { useActionToasts } from '#/components/Toast'
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
  const toast = useActionToasts()

  const [createOpen, setCreateOpen] = useState(false)
  const [createError, setCreateError] = useState('')
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState('')

  const {
    data: services = [],
    isLoading,
    isError,
    error,
    refetch,
  } = useQuery({
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
      toast.ok('Service created')
    },
    onError: (err: unknown) => {
      const msg = errorMessage(err)
      setCreateError(msg)
      toast.err(msg)
    },
  })

  const remove = useMutation({
    mutationFn: (id: string) => runQuery(deleteService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      setDeleteConfirm(null)
      setDeleteError('')
      toast.ok('Service deleted')
    },
    onError: (err: unknown) => {
      const msg = errorMessage(err)
      setDeleteError(msg)
      toast.err(msg)
    },
  })

  const deploy = useMutation({
    mutationFn: (service: Service) =>
      runQuery(createDeployment(service)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      toast.ok('Deployment started')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const stop = useMutation({
    mutationFn: (id: string) => runQuery(stopService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      toast.ok('Service stopped')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const hasServices = services.length > 0

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">control plane</p>
          <h1 className="t-display">Services</h1>
        </div>
        {hasServices ? (
          <span className="badge">
            <span className="tnum">{services.length}</span>{' '}
            service{services.length !== 1 ? 's' : ''}
          </span>
        ) : null}
      </header>

      <div className="stack-lg">
        {canOperateService() ? (
          <section className="panel overflow-hidden">
            <button
              type="button"
              className="flex w-full items-center justify-between px-4 py-3 text-left hover:bg-[var(--surface-2)]"
              aria-expanded={createOpen}
              aria-controls="new-service-panel"
              onClick={() => setCreateOpen((v) => !v)}
            >
              <span className="cluster">
                <span className="signal signal-steady" aria-hidden="true" />
                <span className="kicker">new service</span>
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
                className="border-t border-[var(--border)] panel-pad"
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

        <section>
          {deleteError ? (
            <div style={{ marginBottom: '0.9rem' }}>
              <InlineError message={deleteError} />
            </div>
          ) : null}

          {isLoading ? (
            <SkeletonRows rows={4} />
          ) : isError ? (
            <div className="panel">
              <EmptyState
                icon={<Boxes size={22} />}
                title="Could not load services"
                hint={errorMessage(error)}
                action={
                  <button type="button" className="btn" onClick={() => refetch()}>
                    Retry
                  </button>
                }
              />
            </div>
          ) : !hasServices ? (
            <div className="panel">
              <EmptyState
                icon={<Rocket size={22} />}
                title="No services yet"
                hint="Create a project and deploy your first service. Routes, TLS, and runtime metrics follow automatically."
                action={
                  <Link to="/projects" className="btn btn-primary">
                    Create a project
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
                    <th>routes</th>
                    {canOperateService() ? <th aria-label="actions" /> : null}
                  </tr>
                </thead>
                <tbody>
                  {services.map((svc) => {
                    const status = statusByService.get(svc.id) ?? null
                    return (
                      <tr key={svc.id}>
                        <td>
                          <Link
                            to="/services/$serviceId"
                            params={{ serviceId: svc.id }}
                            className="font-semibold no-underline hover:underline"
                          >
                            {svc.name}
                          </Link>
                        </td>
                        <td>
                          {status ? (
                            <StatusBadge status={status} />
                          ) : (
                            <span className="text-faint">—</span>
                          )}
                        </td>
                        <td className="text-faint" style={{ fontSize: 'var(--text-label)' }}>
                          {svc.domains.length > 0
                            ? svc.domains.join(', ')
                            : `:${svc.internal_port}`}
                        </td>
                        {canOperateService() ? (
                          <td>
                            {canOperate(svc.project_id) ? (
                              <span className="cluster" style={{ justifyContent: 'flex-end' }}>
                                <button
                                  className="btn btn-primary"
                                  type="button"
                                  onClick={() => deploy.mutate(svc)}
                                  disabled={deploy.isPending}
                                >
                                  {deploy.isPending ? 'deploying...' : 'deploy'}
                                </button>
                                <button
                                  className="btn"
                                  type="button"
                                  onClick={() => stop.mutate(svc.id)}
                                  disabled={stop.isPending}
                                >
                                  stop
                                </button>
                                {deleteConfirm === svc.id ? (
                                  <span className="inline-flex items-center gap-1">
                                    <span className="text-[var(--violet)]">delete?</span>
                                    <button
                                      type="button"
                                      className="btn btn-danger"
                                      aria-label="confirm delete service"
                                      onClick={() => remove.mutate(svc.id)}
                                      disabled={remove.isPending}
                                    >
                                      yes
                                    </button>
                                    <button
                                      type="button"
                                      className="btn"
                                      aria-label="cancel delete service"
                                      onClick={() => setDeleteConfirm(null)}
                                    >
                                      no
                                    </button>
                                  </span>
                                ) : (
                                  <button
                                    type="button"
                                    className="btn"
                                    onClick={() => {
                                      setDeleteError('')
                                      setDeleteConfirm(svc.id)
                                    }}
                                  >
                                    delete
                                  </button>
                                )}
                              </span>
                            ) : null}
                          </td>
                        ) : null}
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </main>
  )
}
