import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
} from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'
import { DeployPhase } from '#/components/DeployPhase'
import { TlsToggle } from '#/components/TlsToggle'
import { ServiceForm } from '#/components/ServiceForm'
import { Tabs } from '#/components/Tabs'
import { useAuth, can } from '#/hooks/useAuth'
import { useServiceLogs } from '#/hooks/useServiceLogs'
import type { Deployment, Service, ServiceInput } from '#/effect/schema'

const getService = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getService(id)
  })

const getDeployments = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceDeployments(id)
  })

const getMetrics = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getServiceMetrics(id)
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

const getRequests = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listServiceRequests(id)
  })

const deleteService = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteService(id)
  })

const putService = (input: Service | ServiceInput) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.putService(input)
  })

const listDomains = (serviceId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listDomains(serviceId)
  })

const addDomain = (serviceId: string, hostname: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.addDomain(serviceId, hostname)
  })

const verifyDomain = (serviceId: string, domainId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.verifyDomain(serviceId, domainId)
  })

const deleteDomain = (serviceId: string, domainId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteDomain(serviceId, domainId)
  })

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

const listRegistries = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listRegistries(projectId)
  })

export const Route = createFileRoute('/services/$serviceId')({
  component: ServiceDetail,
})

export function ServiceDetail() {
  const params = useParams({ from: '/services/$serviceId' })
  const id = params.serviceId
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const { isSuperAdmin, roleForActiveProject } = useAuth()
  const [activeTab, setActiveTab] = useState('overview')
  const [editing, setEditing] = useState(false)
  const [editError, setEditError] = useState('')
  const [deleteConfirm, setDeleteConfirm] = useState(false)
  const [deleteError, setDeleteError] = useState('')
  const [domainHostname, setDomainHostname] = useState('')
  const [domainDeleteConfirm, setDomainDeleteConfirm] = useState<string | null>(null)
  const [domainDeleteError, setDomainDeleteError] = useState('')

  const { data: service } = useQuery({
    queryKey: ['services', id],
    queryFn: () => runQuery(getService(id)),
  })

  const { data: projects = [] } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  const isInProgress = (() => {
    const data = (queryClient.getQueryData([
      'services',
      id,
      'deployments',
    ]) as Deployment[] | undefined) ?? []
    if (data.length === 0) return false
    const newest = data.reduce((a, b) => (a.id > b.id ? a : b))
    return ['Pending', 'Building', 'Starting'].includes(newest.status)
  })()

  const {
    data: deployments = [],
    isFetching: deploymentsFetching,
  } = useQuery({
    queryKey: ['services', id, 'deployments'],
    queryFn: () => runQuery(getDeployments(id)),
    refetchInterval: isInProgress ? 2000 : false,
    refetchIntervalInBackground: false,
  })

  const newestDeployment =
    deployments.length > 0
      ? deployments.reduce((a, b) => (a.id > b.id ? a : b))
      : undefined

  const { lines: logs, error: logsError } = useServiceLogs(
    id,
    activeTab === 'logs',
  )

  const { data: metrics = [] } = useQuery({
    queryKey: ['services', id, 'metrics'],
    queryFn: () => runQuery(getMetrics(id)),
    refetchInterval: 3000,
    refetchIntervalInBackground: false,
  })

  const { data: requests = [] } = useQuery({
    queryKey: ['services', id, 'requests'],
    queryFn: () => runQuery(getRequests(id)),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const canOperate = (() => {
    if (isSuperAdmin) return true
    if (!service) return false
    const role = roleForActiveProject(service.project_id)
    return role !== undefined && can('operator', role)
  })()

  const { data: domains = [] } = useQuery({
    queryKey: ['services', id, 'domains'],
    queryFn: () => runQuery(listDomains(id)),
    enabled: canOperate,
  })

  const canManageProject =
    service !== undefined &&
    (isSuperAdmin || roleForActiveProject(service.project_id) === 'admin')

  const { data: registries = [] } = useQuery({
    queryKey: ['projects', service?.project_id, 'registries'],
    queryFn: () => runQuery(listRegistries(service!.project_id)),
    enabled: canManageProject,
  })

  const addDomainMutation = useMutation({
    mutationFn: (hostname: string) => runQuery(addDomain(id, hostname)),
    onSuccess: () => {
      setDomainHostname('')
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'domains'],
      })
    },
  })

  const verifyMutation = useMutation({
    mutationFn: (domainId: string) => runQuery(verifyDomain(id, domainId)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'domains'],
      })
    },
  })

  const deleteDomainMutation = useMutation({
    mutationFn: (domainId: string) => runQuery(deleteDomain(id, domainId)),
    onSuccess: () => {
      setDomainDeleteConfirm(null)
      setDomainDeleteError('')
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'domains'],
      })
    },
    onError: (error: unknown) => {
      const msg = error instanceof Error ? error.message : 'Failed to delete'
      setDomainDeleteError(msg)
      setDomainDeleteConfirm(null)
    },
  })

  const deploy = useMutation({
    mutationFn: (svc: Service) => runQuery(createDeployment(svc)),
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

  const update = useMutation({
    mutationFn: (input: Service | ServiceInput) => runQuery(putService(input)),
    onSuccess: () => {
      setEditing(false)
      setEditError('')
      queryClient.invalidateQueries({ queryKey: ['services', id] })
      queryClient.invalidateQueries({ queryKey: ['services'] })
    },
    onError: (error: unknown) => {
      const msg = error instanceof Error ? error.message : 'Update failed'
      setEditError(msg)
    },
  })

  const remove = useMutation({
    mutationFn: () => runQuery(deleteService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      navigate({ to: '/services' })
    },
    onError: (error: unknown) => {
      const msg = error instanceof Error ? error.message : 'Delete failed'
      setDeleteError(msg)
      setDeleteConfirm(false)
    },
  })

  const newestFirst = deployments

  const tabs = [
    { id: 'overview', label: 'overview' },
    { id: 'source', label: 'source' },
    ...(canOperate ? [{ id: 'domains', label: 'domains' }] : []),
    { id: 'environment', label: 'environment' },
    { id: 'deployments', label: 'deployments' },
    { id: 'logs', label: 'logs' },
    { id: 'metrics', label: 'metrics' },
  ]

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">
        service{' '}
        <Link to="/services" className="text-[var(--fg-muted)]">
          &larr; back
        </Link>
      </p>
      <div className="mb-6 flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold tracking-tight text-[var(--fg)]">
          {service?.name ?? id}
        </h1>
        {canOperate ? (
          <>
            <button
              className="btn btn-primary text-xs"
              type="button"
              onClick={() => service && deploy.mutate(service)}
              disabled={deploy.isPending || !service}
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
            {deleteConfirm ? (
              <span className="inline-flex items-center gap-1 text-xs">
                <span className="text-[var(--violet)]">delete?</span>
                <button
                  type="button"
                  className="btn text-xs"
                  aria-label="confirm delete service"
                  onClick={() => remove.mutate()}
                  disabled={remove.isPending}
                >
                  yes
                </button>
                <button
                  type="button"
                  className="btn text-xs"
                  aria-label="cancel delete service"
                  onClick={() => setDeleteConfirm(false)}
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
                  setDeleteConfirm(true)
                }}
              >
                delete
              </button>
            )}
          </>
        ) : null}
        {service ? <TlsToggle service={service} /> : null}
      </div>

      {deleteError ? (
        <div className="panel mb-4 p-3 text-xs text-[var(--fg)]">
          <span className="signal signal-fault mr-2 inline-block align-middle" />
          {deleteError}
        </div>
      ) : null}

      <Tabs tabs={tabs} active={activeTab} onChange={setActiveTab}>
        {(active) => {
          if (active === 'overview') {
            return (
              <div className="panel overflow-hidden">
                <ul className="m-0 list-none">
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
                    <span className="kicker w-32">name</span>
                    <span className="text-[var(--fg)]">
                      {service?.name ?? '—'}
                    </span>
                  </li>
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
                    <span className="kicker w-32">project</span>
                    <span className="tnum text-[var(--fg)]">
                      {service?.project_id ?? '—'}
                    </span>
                  </li>
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
                    <span className="kicker w-32">internal port</span>
                    <span className="tnum text-[var(--fg)]">
                      {service ? service.internal_port : '—'}
                    </span>
                  </li>
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
                    <span className="kicker w-32">tls</span>
                    <span className="text-[var(--fg)]">
                      {service?.tls_enabled ? 'enabled' : 'disabled'}
                    </span>
                  </li>
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm border-b border-[var(--border)]">
                    <span className="kicker w-32">autoscale</span>
                    <span className="text-[var(--fg)]">
                      {service?.autoscale
                        ? [
                            `${service.autoscale.min_replicas}–${service.autoscale.max_replicas} replicas`,
                            `${service.autoscale.target_cpu_pct}% cpu`,
                            ...(service.autoscale.target_mem_pct != null
                              ? [`${service.autoscale.target_mem_pct}% mem`]
                              : []),
                            ...(service.autoscale.min_replicas === 0
                              ? ['scale-to-zero']
                              : []),
                          ].join(' · ')
                        : 'disabled'}
                    </span>
                  </li>
                  <li className="flex items-center gap-3 px-4 py-2.5 text-sm">
                    <span className="kicker w-32">status</span>
                    {newestDeployment ? (
                      <DeployPhase status={newestDeployment.status} />
                    ) : (
                      <span className="text-[var(--fg-muted)]">
                        no deployments yet
                      </span>
                    )}
                  </li>
                </ul>
              </div>
            )
          }

          if (active === 'source') {
            return (
              <div>
                {service ? (
                  <SourceSummary
                    source={service.source}
                    registries={registries}
                  />
                ) : (
                  <p className="text-sm text-[var(--fg-muted)]">Loading…</p>
                )}
                {canOperate && service ? (
                  <div className="mt-4">
                    {editing ? (
                      <>
                        <div className="mb-3 flex items-center gap-3">
                          <button
                            type="button"
                            className="btn text-xs"
                            onClick={() => {
                              setEditing(false)
                              setEditError('')
                            }}
                          >
                            cancel
                          </button>
                        </div>
                        <div className="panel p-4">
                          <ServiceForm
                            projects={projects.map((p) => ({
                              id: p.id,
                              name: p.name,
                            }))}
                            registries={registries.map((r) => ({
                              id: r.id,
                              name: r.name,
                              project_id: r.project_id,
                              endpoint: r.endpoint,
                            }))}
                            initial={service}
                            submitLabel="save"
                            pending={update.isPending}
                            error={editError || undefined}
                            onSubmit={(value) => update.mutate(value)}
                          />
                        </div>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="btn btn-primary text-xs"
                        onClick={() => {
                          setEditError('')
                          setEditing(true)
                        }}
                      >
                        edit
                      </button>
                    )}
                  </div>
                ) : null}
              </div>
            )
          }

          if (active === 'domains') {
            return (
              <DomainsSection
                domains={domains}
                hostname={domainHostname}
                onHostnameChange={setDomainHostname}
                onAdd={(h) => addDomainMutation.mutate(h)}
                onVerify={(d) => verifyMutation.mutate(d)}
                onDelete={(d) => deleteDomainMutation.mutate(d)}
                addPending={addDomainMutation.isPending}
                verifyPending={verifyMutation.isPending}
                deletePending={deleteDomainMutation.isPending}
                deleteConfirm={domainDeleteConfirm}
                onDeleteConfirm={setDomainDeleteConfirm}
                deleteError={domainDeleteError}
              />
            )
          }

          if (active === 'environment') {
            return service && service.env.length > 0 ? (
              <div className="panel overflow-x-auto">
                <table className="w-full text-left text-sm">
                  <thead>
                    <tr className="border-b border-[var(--border)] text-xs text-[var(--fg-muted)]">
                      <th className="px-4 py-2 font-semibold">key</th>
                      <th className="px-4 py-2 font-semibold">value</th>
                    </tr>
                  </thead>
                  <tbody>
                    {service.env.map(([key, value], i) => (
                      <tr
                        key={key}
                        className={
                          i > 0 ? 'border-t border-[var(--border)]' : ''
                        }
                      >
                        <td className="px-4 py-2 text-xs font-mono text-[var(--fg)]">
                          {key}
                        </td>
                        <td className="px-4 py-2 text-xs font-mono text-[var(--fg)] break-all">
                          {value}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <p className="text-sm text-[var(--fg-muted)]">
                No environment variables.
              </p>
            )
          }

          if (active === 'deployments') {
            return (
              <div>
                <p className="kicker mb-2">
                  deployments{' '}
                  {deploymentsFetching ? (
                    <span className="text-[var(--fg-muted)]">fetching…</span>
                  ) : (
                    <span className="text-[var(--fg-muted)]">
                      {deployments.length}
                    </span>
                  )}
                </p>

                {newestDeployment ? (
                  <div className="mb-4">
                    <DeployPhase status={newestDeployment.status} />
                  </div>
                ) : null}

                {newestDeployment && newestDeployment.artifact ? (
                  <p className="mb-3 flex flex-wrap items-center gap-2 text-xs">
                    <span className="text-[var(--fg-muted)]">artifact:</span>
                    <code className="tnum text-[var(--fg)]">
                      {newestDeployment.artifact.digest.slice(0, 12)}
                    </code>
                    <span className="text-[var(--fg-muted)]">
                      {newestDeployment.artifact.kind === 'OciImage'
                        ? 'image'
                        : 'bundle'}
                    </span>
                  </p>
                ) : newestDeployment ? (
                  <p className="mb-3 text-xs text-[var(--fg-muted)]">
                    artifact:{' '}
                    {newestDeployment.status === 'Failed' ||
                    newestDeployment.status === 'Stopped'
                      ? 'none'
                      : 'pending'}
                  </p>
                ) : null}

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
                          className={
                            i > 0 ? 'border-t border-[var(--border)]' : ''
                          }
                        >
                          <Link
                            to="/deployments/$deploymentId"
                            params={{ deploymentId: d.id }}
                            className="flex items-center gap-4 px-4 py-3 text-sm hover:bg-[var(--bg-elev)]"
                          >
                            <StatusSignal status={d.status} />
                            <span className="tnum text-xs text-[var(--fg-muted)]">
                              {d.created_at}
                            </span>
                          </Link>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )
          }

          if (active === 'logs') {
            return (
              <>
                <div className="mb-2 flex items-center gap-2 text-xs">
                  {logsError ? (
                    <span className="text-[var(--violet)]">
                      <span className="signal signal-fault mr-2 inline-block align-middle" />
                      {logsError}
                    </span>
                  ) : (
                    <>
                      <span className="signal signal-steady" />
                      <span className="kicker">live</span>
                      <span className="tnum text-[var(--fg-muted)]">
                        {logs.length} line{logs.length === 1 ? '' : 's'}
                      </span>
                    </>
                  )}
                </div>
                {logs.length === 0 ? (
                  <p className="text-sm text-[var(--fg-muted)]">
                    {logsError ? 'Stream unavailable.' : 'Waiting for logs...'}
                  </p>
                ) : (
                  <div className="panel overflow-hidden">
                    <ul className="m-0 list-none">
                      {logs.map((line, i) => (
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
                  </div>
                )}
              </>
            )
          }

          // metrics
          return (
            <div>
              <p className="kicker mb-2">recent requests</p>
              {requests.length === 0 ? (
                <p className="mb-8 text-sm text-[var(--fg-muted)]">
                  No recent requests.
                </p>
              ) : (
                <div className="panel mb-8 overflow-x-auto">
                  <table className="w-full text-left text-sm">
                    <thead>
                      <tr className="border-b border-[var(--border)] text-xs text-[var(--fg-muted)]">
                        <th className="px-4 py-2 font-semibold">time</th>
                        <th className="px-4 py-2 font-semibold">method</th>
                        <th className="px-4 py-2 font-semibold">path</th>
                        <th className="px-4 py-2 font-semibold tnum">status</th>
                        <th className="px-4 py-2 font-semibold tnum">bytes</th>
                        <th className="px-4 py-2 font-semibold tnum">
                          duration
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {requests.map((entry, i) => (
                        <tr
                          key={`${entry.recorded_at}-${i}`}
                          className={
                            i > 0 ? 'border-t border-[var(--border)]' : ''
                          }
                        >
                          <td className="px-4 py-2 text-xs text-[var(--fg-muted)] tnum">
                            {entry.recorded_at}
                          </td>
                          <td className="px-4 py-2 text-xs text-[var(--fg)]">
                            {entry.method}
                          </td>
                          <td className="px-4 py-2 text-xs text-[var(--fg)] break-all">
                            {entry.path}
                          </td>
                          <td className="px-4 py-2 tnum text-xs text-[var(--fg)]">
                            {entry.status}
                          </td>
                          <td className="px-4 py-2 tnum text-xs text-[var(--fg-muted)]">
                            {entry.bytes === null
                              ? '—'
                              : formatBytes(entry.bytes)}
                          </td>
                          <td className="px-4 py-2 tnum text-xs text-[var(--fg-muted)]">
                            {entry.duration_ms === null
                              ? '—'
                              : `${entry.duration_ms}ms`}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}

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
                          key={`${m.recorded_at}-${i}`}
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
            </div>
          )
        }}
      </Tabs>
    </main>
  )
}

function SourceSummary({
  source,
  registries,
}: {
  source: Service['source']
  registries: ReadonlyArray<{ id: string; name: string; project_id: string }>
}) {
  const registry =
    source.type === 'external_image' && source.registry_id
      ? registries.find((r) => r.id === source.registry_id)
      : undefined

  const rows: Array<[string, React.ReactNode]> =
    source.type === 'git'
      ? [
          ['type', 'git'],
          ['repo url', source.repo_url],
          ['git ref', source.git_ref],
          ['dockerfile', source.dockerfile_path],
          ['context', source.context_path],
        ]
      : [
          ['type', 'external image'],
          ...(source.image
            ? ([['image', source.image]] as Array<[string, React.ReactNode]>)
            : []),
          ...(source.registry_id
            ? ([
                [
                  'registry',
                  registry ? (
                    <Link
                      to="/projects/$projectId"
                      params={{ projectId: registry.project_id }}
                      className="text-[var(--pink)] no-underline hover:underline"
                    >
                      {registry.name}
                    </Link>
                  ) : (
                    source.registry_id
                  ),
                ],
              ] as Array<[string, React.ReactNode]>)
            : []),
          ...(source.image_ref
            ? ([
                ['image ref', source.image_ref],
              ] as Array<[string, React.ReactNode]>)
            : []),
        ]

  return (
    <div className="panel overflow-hidden">
      <ul className="m-0 list-none">
        {rows.map(([label, value], i) => (
          <li
            key={label}
            className={`flex items-center gap-3 px-4 py-2.5 text-sm ${
              i > 0 ? 'border-t border-[var(--border)]' : ''
            }`}
          >
            <span className="kicker w-32">{label}</span>
            <span className="font-mono text-[var(--fg)] break-all">
              {value}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

function domainSignalClass(status: string): string {
  switch (status) {
    case 'verified':
      return 'signal signal-steady'
    case 'pending':
      return 'signal signal-warn'
    case 'failed':
      return 'signal signal-fault'
    default:
      return 'signal'
  }
}

interface DomainsSectionProps {
  domains: ReadonlyArray<{
    id: string
    hostname: string
    status: string
    last_error: string | null
  }>
  hostname: string
  onHostnameChange: (v: string) => void
  onAdd: (hostname: string) => void
  onVerify: (domainId: string) => void
  onDelete: (domainId: string) => void
  addPending: boolean
  verifyPending: boolean
  deletePending: boolean
  deleteConfirm: string | null
  onDeleteConfirm: (id: string | null) => void
  deleteError: string
}

function DomainsSection({
  domains,
  hostname,
  onHostnameChange,
  onAdd,
  onVerify,
  onDelete,
  addPending,
  verifyPending,
  deletePending,
  deleteConfirm,
  onDeleteConfirm,
  deleteError,
}: DomainsSectionProps) {
  return (
    <div>
      <p className="kicker mb-2">
        domains{' '}
        <span className="text-[var(--fg-muted)]">{domains.length}</span>
      </p>

      {deleteError ? (
        <div className="panel mb-3 p-3 text-xs text-[var(--fg)]">
          <span className="signal signal-fault mr-2 inline-block align-middle" />
          {deleteError}
        </div>
      ) : null}

      {domains.length === 0 ? (
        <p className="mb-3 text-sm text-[var(--fg-muted)]">
          No domains configured.
        </p>
      ) : (
        <div className="panel mb-3 overflow-hidden">
          <ul className="m-0 list-none">
            {domains.map((d, i) => (
              <li
                key={d.id}
                className={`flex items-center gap-4 px-4 py-2.5 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span
                  className={domainSignalClass(d.status)}
                  aria-hidden="true"
                />
                <span className="font-semibold text-[var(--fg)]">
                  {d.hostname}
                </span>
                <span className="kicker">{d.status}</span>
                {d.last_error ? (
                  <span className="text-xs text-[var(--violet)] ml-auto truncate max-w-60">
                    {d.last_error}
                  </span>
                ) : null}

                {d.status === 'pending' || d.status === 'failed' ? (
                  <button
                    type="button"
                    className="btn text-xs ml-auto"
                    onClick={() => onVerify(d.id)}
                    disabled={verifyPending}
                  >
                    {verifyPending ? 'verifying...' : 'verify'}
                  </button>
                ) : null}

                {deleteConfirm === d.id ? (
                  <span className="inline-flex items-center gap-1 text-xs">
                    <span className="text-[var(--violet)]">remove?</span>
                    <button
                      type="button"
                      className="btn text-xs"
                      aria-label="confirm remove domain"
                      onClick={() => {
                        onDelete(d.id)
                      }}
                      disabled={deletePending}
                    >
                      yes
                    </button>
                    <button
                      type="button"
                      className="btn text-xs"
                      aria-label="cancel remove domain"
                      onClick={() => onDeleteConfirm(null)}
                    >
                      no
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn text-xs ml-auto"
                    onClick={() => onDeleteConfirm(d.id)}
                  >
                    delete
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault()
          const trimmed = hostname.trim()
          if (!trimmed) return
          onAdd(trimmed)
        }}
      >
        <input
          type="text"
          aria-label="domain hostname"
          placeholder="hostname"
          value={hostname}
          onChange={(e) => onHostnameChange(e.target.value)}
          className="field-input"
        />
        <button
          type="submit"
          className="btn btn-primary text-xs"
          disabled={addPending || hostname.trim().length === 0}
        >
          {addPending ? 'adding...' : 'add domain'}
        </button>
      </form>
    </div>
  )
}

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GiB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MiB`
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${bytes} B`
}
