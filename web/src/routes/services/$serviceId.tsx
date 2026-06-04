import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
} from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { Boxes, Globe, KeyRound } from 'lucide-react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { effectiveEndpoints } from '#/effect/schema'
import { runQuery } from '#/effect/runtime'
import { StatusBadge } from '#/components/StatusBadge'
import { DeployPhase } from '#/components/DeployPhase'
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
import { LogStream } from '#/components/LogStream'
import { ServiceConsole } from '#/components/ServiceConsole'
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
  MetricSnapshot,
  Service,
  ServiceInput,
} from '#/effect/schema'
import type { ServiceEndpoint } from '#/effect/schema'

// One client-side metrics sample: a percentage derived from successive
// `cpu_usage_usec` deltas plus the instantaneous memory reading and the wall
// clock at which we observed it. The backend has no time series, so the chart
// is built from samples accumulated while the tab is open.
interface MetricsSample {
  readonly cpuPct: number
  readonly memoryBytes: number
  readonly at: number
}

const METRICS_HISTORY = 40

// Deployment statuses that warrant fast polling on the deployments timeline.
const DEPLOY_IN_PROGRESS = ['Pending', 'Building', 'Starting']

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

  const {
    data: deployments = [],
    isLoading: deploymentsLoading,
  } = useQuery({
    queryKey: ['services', id, 'deployments'],
    queryFn: () => runQuery(getDeployments(id)),
    // Poll fast only while the newest deployment is mid-flight. Derived from the
    // query's own data (passed to the callback) rather than a cache read during
    // render, so the interval reacts as soon as fresh data lands.
    refetchInterval: (query) => {
      const data = query.state.data ?? []
      if (data.length === 0) return false
      const newest = data.reduce((a, b) => (a.id > b.id ? a : b))
      return DEPLOY_IN_PROGRESS.includes(newest.status) ? 2000 : false
    },
    refetchIntervalInBackground: false,
  })

  const newestDeployment =
    deployments.length > 0
      ? deployments.reduce((a, b) => (a.id > b.id ? a : b))
      : undefined

  const { data: metrics = [], isLoading: metricsLoading } = useQuery({
    queryKey: ['services', id, 'metrics'],
    queryFn: () => runQuery(getMetrics(id)),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const metricsHistory = useServiceMetricsHistory(metrics)

  const canOperate = (() => {
    if (isSuperAdmin) return true
    if (!service) return false
    const role = roleForActiveProject(service.project_id)
    return role !== undefined && can('operator', role)
  })()

  // The access-log endpoint requires Operator (src/api/observability.rs); gating
  // the query keeps a read-only viewer from triggering a spurious 403 when they
  // open the metrics tab.
  const { data: requests = [], isLoading: requestsLoading } = useQuery({
    queryKey: ['services', id, 'requests'],
    queryFn: () => runQuery(getRequests(id)),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
    enabled: canOperate,
  })

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
    ...(canOperate ? [{ id: 'console', label: 'console' }] : []),
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

                {service ? <EndpointsPanel service={service} /> : null}

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

                {/* Phase line for the active (newest, non-terminal) deployment. */}
                {newestDeployment &&
                DEPLOY_IN_PROGRESS.includes(newestDeployment.status) ? (
                  <div className="panel panel-pad">
                    <DeployPhase status={newestDeployment.status} />
                  </div>
                ) : null}

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
                              <span className="cluster" style={{ gap: '0.5rem' }}>
                                <Link
                                  to="/deployments/$deploymentId"
                                  params={{ deploymentId: d.id }}
                                  className="no-underline"
                                >
                                  <StatusBadge status={d.status} />
                                </Link>
                                {d.status === 'Healthy' ? (
                                  <span className="badge badge-ok">current</span>
                                ) : null}
                              </span>
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
                <LogStream
                  path={`/v1/services/${id}/logs/stream`}
                  title="logs"
                  showLineNumbers
                  height="32rem"
                />
              </div>
            )
          }

          if (active === 'console') {
            return <ServiceConsole serviceId={id} />
          }

          // metrics
          return (
            <div className="stack-lg">
              <MetricsCharts
                latest={metrics[0]}
                history={metricsHistory}
                loading={metricsLoading}
              />

              {canOperate ? (
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
              ) : null}
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

// Network endpoints (ADR-036). Shows the service's effective endpoints: the
// projected default http endpoint for legacy services, or the explicit
// http/tcp/udp list. Public ports are Denia-allocated; until the allocator
// persists one a tcp/udp endpoint reads "pending", and http is reached via its
// domain rather than a public port.
function EndpointsPanel({ service }: { service: Service }) {
  const endpoints = effectiveEndpoints(service)
  const protocolBadge = (protocol: ServiceEndpoint['protocol']) =>
    protocol === 'http' ? 'badge badge-steady' : 'badge'
  return (
    <section className="stack">
      <p className="kicker">
        endpoints <span className="text-faint tnum">{endpoints.length}</span>
      </p>
      <div className="panel overflow-hidden">
        <table className="dtable">
          <thead>
            <tr>
              <th>name</th>
              <th>protocol</th>
              <th className="num">internal</th>
              <th className="num">public</th>
            </tr>
          </thead>
          <tbody>
            {endpoints.map((ep) => (
              <tr key={`${ep.protocol}-${ep.name}-${ep.internal_port}`}>
                <td className="font-mono">{ep.name}</td>
                <td>
                  <span className={protocolBadge(ep.protocol)}>
                    {ep.protocol}
                  </span>
                </td>
                <td className="num tnum">:{ep.internal_port}</td>
                <td className="num">
                  {ep.protocol === 'http' ? (
                    <span className="text-faint">via domain</span>
                  ) : ep.public_port != null ? (
                    <span className="tnum">:{ep.public_port}</span>
                  ) : (
                    <span className="badge badge-warn">pending</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
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

// The backend `/metrics` endpoint returns at most ONE current snapshot with
// cumulative `cpu_usage_usec` (microseconds of CPU time since the cgroup was
// created) and instantaneous `memory_current_bytes`. There is no server-side
// time series, so we derive an instantaneous CPU% from the delta between two
// successive polls (busy CPU-time over wall-clock elapsed) and accumulate a
// short client-side history for the trend chart. Mirrors the node-gauge delta
// approach used on the dashboard.
function useServiceMetricsHistory(
  metrics: ReadonlyArray<MetricSnapshot>,
): ReadonlyArray<MetricsSample> {
  const [series, setSeries] = useState<ReadonlyArray<MetricsSample>>([])
  const prev = useRef<{ cpuUsec: number; at: number } | null>(null)

  const latest = metrics.length > 0 ? metrics[0] : undefined

  useEffect(() => {
    if (!latest) {
      prev.current = null
      setSeries([])
      return
    }
    const now = Date.now()
    const last = prev.current
    if (last && now > last.at) {
      const dCpuUsec = Math.max(0, latest.cpu_usage_usec - last.cpuUsec)
      const elapsedUsec = (now - last.at) * 1000
      const cpuPct =
        elapsedUsec > 0
          ? Math.max(0, Math.min(100, (dCpuUsec / elapsedUsec) * 100))
          : 0
      setSeries((s) =>
        [
          ...s,
          { cpuPct, memoryBytes: latest.memory_current_bytes, at: now },
        ].slice(-METRICS_HISTORY),
      )
    }
    prev.current = { cpuUsec: latest.cpu_usage_usec, at: now }
    // Keying on the cumulative counter + memory means a fresh poll (even with an
    // identical counter) advances the series via the wall-clock delta.
  }, [latest?.cpu_usage_usec, latest?.memory_current_bytes, latest])

  return series
}

function MetricsCharts({
  latest,
  history,
  loading,
}: {
  latest: MetricSnapshot | undefined
  history: ReadonlyArray<MetricsSample>
  loading: boolean
}) {
  if (loading && !latest) {
    return <SkeletonRows rows={4} />
  }
  if (!latest) {
    return (
      <div className="panel">
        <EmptyState
          title="No metrics yet"
          hint="CPU and memory samples appear here once the workload is running."
        />
      </div>
    )
  }

  const xLabels = history.map((s) => formatClock(new Date(s.at).toISOString()))
  const cpuValues = history.map((s) => s.cpuPct)
  const memValues = history.map((s) => s.memoryBytes)
  // The current CPU% is the most recent derived sample (none yet on first poll).
  const currentCpu = history.length > 0 ? history[history.length - 1].cpuPct : null

  return (
    <div className="stack-lg">
      <section className="stack">
        <div className="panel-head">
          <p className="kicker">cpu</p>
          <span className="t-title tnum">
            {currentCpu === null ? '—' : formatPercent(currentCpu, 1)}
          </span>
        </div>
        <div className="panel panel-pad">
          {cpuValues.length > 1 ? (
            <AreaChart
              series={[
                { label: 'cpu', color: 'var(--pink)', values: cpuValues },
              ]}
              xLabels={xLabels}
              yFormat={(v) => formatPercent(v, 0)}
            />
          ) : (
            <p className="text-faint">Sampling… the CPU trend builds from successive readings.</p>
          )}
        </div>
      </section>

      <section className="stack">
        <div className="panel-head">
          <p className="kicker">memory</p>
          <span className="t-title tnum">
            {formatBytes(latest.memory_current_bytes)}
          </span>
        </div>
        <div className="panel panel-pad">
          {memValues.length > 1 ? (
            <AreaChart
              series={[
                { label: 'memory', color: 'var(--violet)', values: memValues },
              ]}
              xLabels={xLabels}
              yFormat={(v) => formatBytes(v)}
            />
          ) : (
            <p className="text-faint">Sampling… the memory trend builds from successive readings.</p>
          )}
        </div>
      </section>

      {/* Tabular samples back the charts with exact figures. */}
      {history.length > 0 ? (
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
                {history
                  .slice()
                  .reverse()
                  .map((s) => (
                    <tr key={s.at}>
                      <td className="text-faint">
                        <Num>{formatClock(new Date(s.at).toISOString())}</Num>
                      </td>
                      <td className="num">
                        <Num>{formatPercent(s.cpuPct, 1)}</Num>
                      </td>
                      <td className="num">
                        <Num>{formatBytes(s.memoryBytes)}</Num>
                      </td>
                    </tr>
                  ))}
              </tbody>
            </table>
          </div>
        </section>
      ) : null}
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
