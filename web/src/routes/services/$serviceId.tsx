import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
} from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Boxes, Globe, KeyRound } from 'lucide-react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusBadge } from '#/components/StatusBadge'
import { TlsToggle } from '#/components/TlsToggle'
import { ServiceForm } from '#/components/ServiceForm'
import { Tabs } from '#/components/Tabs'
import { AreaChart } from '#/components/Charts'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { ErrorPanel, InlineError, errorMessage } from '#/components/ErrorPanel'
import { useActionToasts } from '#/components/Toast'
import { Num } from '#/components/Num'
import { useAuth, can } from '#/hooks/useAuth'
import { useServiceLogs } from '#/hooks/useServiceLogs'
import {
  formatBytes,
  formatClock,
  formatDuration,
  formatMillis,
  formatPercent,
  formatRelative,
  shortId,
} from '#/lib/format'
import type {
  Deployment,
  MetricSnapshot,
  Service,
  ServiceInput,
} from '#/effect/schema'

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

function statusFor(status: number): string {
  if (status >= 500) return 'Failed'
  if (status >= 400) return 'Pending'
  return 'Healthy'
}

export function ServiceDetail() {
  const params = useParams({ from: '/services/$serviceId' })
  const id = params.serviceId
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const { isSuperAdmin, roleForActiveProject } = useAuth()
  const toast = useActionToasts()
  const [activeTab, setActiveTab] = useState('overview')
  const [editing, setEditing] = useState(false)
  const [editError, setEditError] = useState('')
  const [deleteConfirm, setDeleteConfirm] = useState(false)
  const [deleteError, setDeleteError] = useState('')
  const [domainHostname, setDomainHostname] = useState('')
  const [domainDeleteConfirm, setDomainDeleteConfirm] = useState<string | null>(null)
  const [domainDeleteError, setDomainDeleteError] = useState('')

  const {
    data: service,
    isLoading: serviceLoading,
    isError: serviceError,
    error: serviceErr,
  } = useQuery({
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
    isLoading: deploymentsLoading,
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

  const { data: metrics = [], isLoading: metricsLoading } = useQuery({
    queryKey: ['services', id, 'metrics'],
    queryFn: () => runQuery(getMetrics(id)),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const { data: requests = [], isLoading: requestsLoading } = useQuery({
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
      toast.ok('Domain added')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const verifyMutation = useMutation({
    mutationFn: (domainId: string) => runQuery(verifyDomain(id, domainId)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'domains'],
      })
      toast.ok('Verification triggered')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const deleteDomainMutation = useMutation({
    mutationFn: (domainId: string) => runQuery(deleteDomain(id, domainId)),
    onSuccess: () => {
      setDomainDeleteConfirm(null)
      setDomainDeleteError('')
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'domains'],
      })
      toast.ok('Domain removed')
    },
    onError: (error: unknown) => {
      const msg = errorMessage(error)
      setDomainDeleteError(msg)
      setDomainDeleteConfirm(null)
      toast.err(msg)
    },
  })

  const deploy = useMutation({
    mutationFn: (svc: Service) => runQuery(createDeployment(svc)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'deployments'],
      })
      toast.ok('Deployment started')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const stop = useMutation({
    mutationFn: () => runQuery(stopService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['services', id, 'deployments'],
      })
      toast.ok('Service stopped')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const update = useMutation({
    mutationFn: (input: Service | ServiceInput) => runQuery(putService(input)),
    onSuccess: () => {
      setEditing(false)
      setEditError('')
      queryClient.invalidateQueries({ queryKey: ['services', id] })
      queryClient.invalidateQueries({ queryKey: ['services'] })
      toast.ok('Service updated')
    },
    onError: (error: unknown) => {
      const msg = errorMessage(error)
      setEditError(msg)
      toast.err(msg)
    },
  })

  const remove = useMutation({
    mutationFn: () => runQuery(deleteService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      toast.ok('Service deleted')
      navigate({ to: '/services' })
    },
    onError: (error: unknown) => {
      const msg = errorMessage(error)
      setDeleteError(msg)
      setDeleteConfirm(false)
      toast.err(msg)
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
    <main className="page-wrap px-4 pb-16 pt-10">
      <p className="kicker mb-3">
        <Link to="/services" className="text-[var(--fg-muted)] no-underline hover:underline">
          &larr; services
        </Link>
      </p>
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">service</p>
          <h1 className="t-display">{service?.name ?? id}</h1>
        </div>
        {canOperate ? (
          <div className="cluster">
            <button
              className="btn btn-primary"
              type="button"
              onClick={() => service && deploy.mutate(service)}
              disabled={deploy.isPending || !service}
            >
              {deploy.isPending ? 'deploying...' : 'deploy'}
            </button>
            <button
              className="btn"
              type="button"
              onClick={() => stop.mutate()}
              disabled={stop.isPending}
            >
              stop
            </button>
            {deleteConfirm ? (
              <span className="inline-flex items-center gap-1">
                <span className="text-[var(--violet)]">delete?</span>
                <button
                  type="button"
                  className="btn btn-danger"
                  aria-label="confirm delete service"
                  onClick={() => remove.mutate()}
                  disabled={remove.isPending}
                >
                  yes
                </button>
                <button
                  type="button"
                  className="btn"
                  aria-label="cancel delete service"
                  onClick={() => setDeleteConfirm(false)}
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
                  setDeleteConfirm(true)
                }}
              >
                delete
              </button>
            )}
            {service ? <TlsToggle service={service} /> : null}
          </div>
        ) : service ? (
          <TlsToggle service={service} />
        ) : null}
      </header>

      {deleteError ? (
        <div style={{ marginBottom: '1rem' }}>
          <InlineError message={deleteError} />
        </div>
      ) : null}

      {serviceError && !service ? (
        <div style={{ marginBottom: '1rem' }}>
          <ErrorPanel
            title="Could not load service"
            message={errorMessage(serviceErr)}
          />
        </div>
      ) : null}

      <Tabs tabs={tabs} active={activeTab} onChange={setActiveTab}>
        {(active) => {
          if (active === 'overview') {
            return (
              <div className="stack-lg">
                <div className="panel panel-pad">
                  <dl className="flex flex-col gap-3" style={{ margin: 0 }}>
                    <KvRow label="name" value={service?.name ?? '—'} />
                    <KvRow
                      label="project"
                      value={
                        service ? (
                          <Num title={service.project_id}>
                            {shortId(service.project_id)}
                          </Num>
                        ) : (
                          '—'
                        )
                      }
                    />
                    <KvRow
                      label="internal port"
                      value={service ? <Num>{service.internal_port}</Num> : '—'}
                    />
                    <KvRow
                      label="tls"
                      value={service?.tls_enabled ? 'enabled' : 'disabled'}
                    />
                    <KvRow
                      label="autoscale"
                      value={
                        service?.autoscale
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
                          : 'disabled'
                      }
                    />
                    <KvRow
                      label="status"
                      value={
                        newestDeployment ? (
                          <StatusBadge status={newestDeployment.status} />
                        ) : (
                          <span className="text-faint">no deployments yet</span>
                        )
                      }
                    />
                  </dl>
                </div>

                <AutoscalePanel autoscale={service?.autoscale ?? null} />
              </div>
            )
          }

          if (active === 'source') {
            return (
              <div className="stack">
                {serviceLoading && !service ? (
                  <SkeletonRows rows={4} />
                ) : service ? (
                  <SourceSummary
                    source={service.source}
                    registries={registries}
                  />
                ) : (
                  <EmptyState title="Source unavailable" hint="The service could not be loaded." />
                )}
                {canOperate && service ? (
                  <div>
                    {editing ? (
                      <>
                        <div className="cluster" style={{ marginBottom: '0.75rem' }}>
                          <button
                            type="button"
                            className="btn"
                            onClick={() => {
                              setEditing(false)
                              setEditError('')
                            }}
                          >
                            cancel
                          </button>
                        </div>
                        <div className="panel panel-pad">
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
                        className="btn btn-primary"
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
              <div className="panel overflow-hidden">
                <table className="dtable">
                  <thead>
                    <tr>
                      <th>key</th>
                      <th>value</th>
                    </tr>
                  </thead>
                  <tbody>
                    {service.env.map(([key, value]) => (
                      <tr key={key}>
                        <td className="font-mono">{key}</td>
                        <td className="font-mono break-all">{value}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="panel">
                <EmptyState
                  icon={<KeyRound size={22} />}
                  title="No environment variables"
                  hint="Variables set on this service appear here. Edit the service to add some."
                />
              </div>
            )
          }

          if (active === 'deployments') {
            return (
              <div className="stack">
                <p className="kicker">
                  deployments{' '}
                  <span className="text-faint tnum">{deployments.length}</span>
                </p>

                {newestDeployment && newestDeployment.artifact ? (
                  <p className="cluster" style={{ fontSize: 'var(--text-label)' }}>
                    <span className="text-faint">artifact</span>
                    <code className="tnum">
                      {newestDeployment.artifact.digest.slice(0, 12)}
                    </code>
                    <span className="text-faint">
                      {newestDeployment.artifact.kind === 'OciImage'
                        ? 'image'
                        : 'bundle'}
                    </span>
                  </p>
                ) : newestDeployment ? (
                  <p className="text-faint" style={{ fontSize: 'var(--text-label)' }}>
                    artifact:{' '}
                    {newestDeployment.status === 'Failed' ||
                    newestDeployment.status === 'Stopped'
                      ? 'none'
                      : 'pending'}
                  </p>
                ) : null}

                {deploymentsLoading && newestFirst.length === 0 ? (
                  <SkeletonRows rows={3} />
                ) : newestFirst.length === 0 ? (
                  <div className="panel">
                    <EmptyState
                      icon={<Boxes size={22} />}
                      title="No deployments yet"
                      hint="Deploy this service to see build and runtime history here."
                    />
                  </div>
                ) : (
                  <div className="panel overflow-hidden">
                    <table className="dtable">
                      <thead>
                        <tr>
                          <th>status</th>
                          <th>id</th>
                          <th>created</th>
                        </tr>
                      </thead>
                      <tbody>
                        {newestFirst.map((d) => (
                          <tr key={d.id}>
                            <td>
                              <Link
                                to="/deployments/$deploymentId"
                                params={{ deploymentId: d.id }}
                                className="no-underline"
                              >
                                <StatusBadge status={d.status} />
                              </Link>
                            </td>
                            <td>
                              <Num className="text-faint">{shortId(d.id)}</Num>
                            </td>
                            <td className="text-faint">
                              <Num>{formatRelative(d.created_at, Date.now())}</Num>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </div>
            )
          }

          if (active === 'logs') {
            return (
              <div className="stack">
                <div className="cluster" style={{ fontSize: 'var(--text-label)' }}>
                  {logsError ? (
                    <InlineError message={logsError} />
                  ) : (
                    <>
                      <span className="signal signal-steady" aria-hidden="true" />
                      <span className="kicker">live</span>
                      <span className="text-faint tnum">
                        {logs.length} line{logs.length === 1 ? '' : 's'}
                      </span>
                    </>
                  )}
                </div>
                {logs.length === 0 ? (
                  <div className="panel">
                    <EmptyState
                      title={logsError ? 'Stream unavailable' : 'Waiting for logs'}
                      hint={
                        logsError
                          ? 'The log stream could not be reached.'
                          : 'Lines appear here as the workload produces them.'
                      }
                    />
                  </div>
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
                          <span className="tnum flex-shrink-0 text-[var(--fg-faint)]">
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
              </div>
            )
          }

          // metrics
          return (
            <div className="stack-lg">
              <MetricsCharts metrics={metrics} loading={metricsLoading} />

              <section className="stack">
                <p className="kicker">access log</p>
                {requestsLoading && requests.length === 0 ? (
                  <SkeletonRows rows={4} />
                ) : requests.length === 0 ? (
                  <div className="panel">
                    <EmptyState
                      icon={<Globe size={22} />}
                      title="No recent requests"
                      hint="Inbound requests proxied to this service appear here."
                    />
                  </div>
                ) : (
                  <div className="panel overflow-hidden">
                    <table className="dtable">
                      <thead>
                        <tr>
                          <th>method</th>
                          <th>path</th>
                          <th>status</th>
                          <th className="num">bytes</th>
                          <th className="num">duration</th>
                          <th className="num">time</th>
                        </tr>
                      </thead>
                      <tbody>
                        {requests.map((entry, i) => (
                          <tr key={`${entry.recorded_at}-${i}`}>
                            <td className="font-mono">{entry.method}</td>
                            <td className="font-mono break-all">{entry.path}</td>
                            <td>
                              <StatusBadge
                                status={statusFor(entry.status)}
                                label={String(entry.status)}
                              />
                            </td>
                            <td className="num text-faint">
                              <Num>
                                {entry.bytes === null ? '—' : formatBytes(entry.bytes)}
                              </Num>
                            </td>
                            <td className="num text-faint">
                              <Num>
                                {entry.duration_ms === null
                                  ? '—'
                                  : formatMillis(entry.duration_ms)}
                              </Num>
                            </td>
                            <td className="num text-faint">
                              <Num>{formatRelative(entry.recorded_at, Date.now())}</Num>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </section>
            </div>
          )
        }}
      </Tabs>
    </main>
  )
}

// Definition row: a fixed-width kicker label beside a tabular value. Mirrors
// the Dashboard `Detail` idiom (flat, no nested card, columns line up).
function KvRow({
  label,
  value,
}: {
  label: string
  value: React.ReactNode
}) {
  return (
    <div className="flex items-baseline gap-4">
      <dt className="kicker" style={{ minWidth: '11rem', flexShrink: 0 }}>
        {label}
      </dt>
      <dd className="tnum" style={{ margin: 0 }}>
        {value}
      </dd>
    </div>
  )
}

function AutoscalePanel({
  autoscale,
}: {
  autoscale: Service['autoscale'] | null | undefined
}) {
  if (!autoscale) {
    return (
      <div className="panel panel-pad">
        <p className="kicker" style={{ marginBottom: '0.6rem' }}>
          autoscaling
        </p>
        <p className="text-faint">Autoscaling disabled (single replica).</p>
      </div>
    )
  }
  return (
    <div className="panel panel-pad">
      <p className="kicker" style={{ marginBottom: '0.6rem' }}>
        autoscaling
      </p>
      <dl className="flex flex-col gap-3" style={{ margin: 0 }}>
        <KvRow
          label="replicas"
          value={
            <Num>
              {autoscale.min_replicas}–{autoscale.max_replicas}
            </Num>
          }
        />
        <KvRow
          label="target cpu"
          value={<Num>{formatPercent(autoscale.target_cpu_pct, 0)}</Num>}
        />
        <KvRow
          label="target mem"
          value={
            autoscale.target_mem_pct != null ? (
              <Num>{formatPercent(autoscale.target_mem_pct, 0)}</Num>
            ) : (
              <span className="text-faint">—</span>
            )
          }
        />
        <KvRow
          label="scale-down cooldown"
          value={<Num>{formatDuration(autoscale.scale_down_cooldown_s)}</Num>}
        />
        <KvRow
          label="idle timeout"
          value={<Num>{formatDuration(autoscale.idle_timeout_s)}</Num>}
        />
        {autoscale.min_replicas === 0 ? (
          <KvRow
            label="scale to zero"
            value={<span className="badge badge-steady">enabled</span>}
          />
        ) : null}
      </dl>
    </div>
  )
}

function MetricsCharts({
  metrics,
  loading,
}: {
  metrics: ReadonlyArray<MetricSnapshot>
  loading: boolean
}) {
  if (loading && metrics.length === 0) {
    return <SkeletonRows rows={4} />
  }
  if (metrics.length === 0) {
    return (
      <div className="panel">
        <EmptyState
          title="No metrics yet"
          hint="CPU and memory samples appear here once the workload is running."
        />
      </div>
    )
  }

  const xLabels = metrics.map((m) => formatClock(m.recorded_at))
  // cpu_percent is a 0..1 fraction on the wire; render as whole percent.
  const cpuValues = metrics.map((m) => m.cpu_percent * 100)
  const memValues = metrics.map((m) => m.memory_bytes)
  const latest = metrics[metrics.length - 1]

  return (
    <div className="stack-lg">
      <section className="stack">
        <div className="panel-head">
          <p className="kicker">cpu</p>
          <span className="t-title tnum">
            {formatPercent(latest.cpu_percent * 100, 1)}
          </span>
        </div>
        <div className="panel panel-pad">
          <AreaChart
            series={[
              { label: 'cpu', color: 'var(--pink)', values: cpuValues },
            ]}
            xLabels={xLabels}
            yFormat={(v) => formatPercent(v, 0)}
          />
        </div>
      </section>

      <section className="stack">
        <div className="panel-head">
          <p className="kicker">memory</p>
          <span className="t-title tnum">{formatBytes(latest.memory_bytes)}</span>
        </div>
        <div className="panel panel-pad">
          <AreaChart
            series={[
              { label: 'memory', color: 'var(--violet)', values: memValues },
            ]}
            xLabels={xLabels}
            yFormat={(v) => formatBytes(v)}
          />
        </div>
      </section>

      {/* Tabular samples back the charts with exact figures. */}
      <section className="stack">
        <p className="kicker">samples</p>
        <div className="panel overflow-hidden">
          <table className="dtable">
            <thead>
              <tr>
                <th>time</th>
                <th className="num">cpu</th>
                <th className="num">memory</th>
              </tr>
            </thead>
            <tbody>
              {metrics.map((m, i) => (
                <tr key={`${m.recorded_at}-${i}`}>
                  <td className="text-faint">
                    <Num>{formatClock(m.recorded_at)}</Num>
                  </td>
                  <td className="num">
                    <Num>{formatPercent(m.cpu_percent * 100, 1)}</Num>
                  </td>
                  <td className="num">
                    <Num>{formatBytes(m.memory_bytes)}</Num>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>
    </div>
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
      <table className="dtable">
        <tbody>
          {rows.map(([label, value]) => (
            <tr key={label}>
              <td className="kicker" style={{ width: '10rem' }}>
                {label}
              </td>
              <td className="font-mono break-all">{value}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
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
    <div className="stack">
      <p className="kicker">
        domains <span className="text-faint tnum">{domains.length}</span>
      </p>

      {deleteError ? <InlineError message={deleteError} /> : null}

      {domains.length === 0 ? (
        <div className="panel">
          <EmptyState
            icon={<Globe size={22} />}
            title="No domains configured"
            hint="Add a hostname below to route traffic to this service."
          />
        </div>
      ) : (
        <div className="panel overflow-hidden">
          <table className="dtable">
            <thead>
              <tr>
                <th>hostname</th>
                <th>status</th>
                <th aria-label="actions" />
              </tr>
            </thead>
            <tbody>
              {domains.map((d) => (
                <tr key={d.id}>
                  <td className="font-mono break-all">{d.hostname}</td>
                  <td>
                    <span className="cluster">
                      <StatusBadge kind="domain" status={d.status} />
                      {d.last_error ? (
                        <span className="text-[var(--violet)] truncate max-w-60" style={{ fontSize: 'var(--text-label)' }}>
                          {d.last_error}
                        </span>
                      ) : null}
                    </span>
                  </td>
                  <td>
                    <span className="cluster" style={{ justifyContent: 'flex-end' }}>
                      {d.status === 'pending' || d.status === 'failed' ? (
                        <button
                          type="button"
                          className="btn"
                          onClick={() => onVerify(d.id)}
                          disabled={verifyPending}
                        >
                          {verifyPending ? 'verifying...' : 'verify'}
                        </button>
                      ) : null}

                      {deleteConfirm === d.id ? (
                        <span className="inline-flex items-center gap-1">
                          <span className="text-[var(--violet)]">remove?</span>
                          <button
                            type="button"
                            className="btn btn-danger"
                            aria-label="confirm remove domain"
                            onClick={() => onDelete(d.id)}
                            disabled={deletePending}
                          >
                            yes
                          </button>
                          <button
                            type="button"
                            className="btn"
                            aria-label="cancel remove domain"
                            onClick={() => onDeleteConfirm(null)}
                          >
                            no
                          </button>
                        </span>
                      ) : (
                        <button
                          type="button"
                          className="btn"
                          onClick={() => onDeleteConfirm(d.id)}
                        >
                          delete
                        </button>
                      )}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <form
        className="cluster"
        style={{ alignItems: 'flex-end' }}
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
          className="btn btn-primary"
          disabled={addPending || hostname.trim().length === 0}
        >
          {addPending ? 'adding...' : 'add domain'}
        </button>
      </form>
    </div>
  )
}
