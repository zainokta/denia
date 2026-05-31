import { createFileRoute, Link } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ListChecks, Timer } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth, can } from '#/hooks/useAuth'
import { useActiveProject } from '#/hooks/useActiveProject'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { Num } from '#/components/Num'
import { formatRelative } from '#/lib/format'

const listJobs = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listJobs(projectId)
  })

const createJob = (input: {
  project_id: string
  name: string
  source: {
    type: 'external_image'
    image: string
    credential: null
    registry_id: string | null
    image_ref: string | null
  }
  command: string[] | null
  env: Array<[string, string]>
  schedule: string | null
  max_retries: number
}) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createJob(input)
  })

const listRegistries = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listRegistries(projectId)
  })

function cronHint(cron: string): string | null {
  if (cron === '*/1 * * * *') return 'every minute'
  if (cron === '*/5 * * * *') return 'every 5 min'
  if (cron === '*/15 * * * *') return 'every 15 min'
  if (cron === '*/30 * * * *') return 'every 30 min'
  if (cron.startsWith('*/')) {
    const m = cron.match(/^\*\/(\d+)/)
    if (m) return `every ${m[1]} min`
  }
  if (/^\d+ \d+ \* \* \*$/.test(cron)) {
    const [min, hour] = cron.split(' ')
    return `daily at ${hour.padStart(2, '0')}:${min.padStart(2, '0')} UTC`
  }
  return null
}

function sourceDisplay(source: unknown): string {
  const s = source as Record<string, unknown>
  if (s.type === 'external_image') return String(s.image)
  if (s.type === 'git') return String(s.repo_url)
  return JSON.stringify(source)
}

export const Route = createFileRoute('/jobs/')({
  component: JobsIndex,
})

export function JobsIndex() {
  const queryClient = useQueryClient()
  const auth = useAuth()
  const [projectId] = useActiveProject()

  const [name, setName] = useState('')
  const [imageMode, setImageMode] = useState<'direct' | 'registry'>('direct')
  const [image, setImage] = useState('')
  const [registryId, setRegistryId] = useState('')
  const [imageRef, setImageRef] = useState('')
  const [command, setCommand] = useState('')
  const [envStr, setEnvStr] = useState('')
  const [schedule, setSchedule] = useState('')
  const [maxRetries, setMaxRetries] = useState(0)
  const [createError, setCreateError] = useState('')

  const {
    data: jobs = [],
    isFetching,
    isLoading,
    isError,
    error,
  } = useQuery({
    queryKey: ['jobs', projectId],
    queryFn: () => runQuery(listJobs(projectId)),
    enabled: projectId.length > 0,
  })

  const userRole = projectId ? auth.roleForActiveProject(projectId) : undefined
  const canOperate = userRole ? can('operator', userRole) : false
  const canManage = auth.isSuperAdmin || userRole === 'admin'

  const { data: registries = [] } = useQuery({
    queryKey: ['projects', projectId, 'registries'],
    queryFn: () => runQuery(listRegistries(projectId)),
    enabled: projectId.length > 0 && canManage,
  })

  const create = useMutation({
    mutationFn: () => {
      const env: Array<[string, string]> = envStr
        .trim()
        .split('\n')
        .filter((l) => l.includes('='))
        .map((l) => {
          const idx = l.indexOf('=')
          return [l.slice(0, idx), l.slice(idx + 1)] as [string, string]
        })
      const source =
        imageMode === 'registry'
          ? {
              type: 'external_image' as const,
              image: '',
              credential: null,
              registry_id: registryId.trim(),
              image_ref: imageRef.trim(),
            }
          : {
              type: 'external_image' as const,
              image: image.trim(),
              credential: null,
              registry_id: null,
              image_ref: null,
            }
      return runQuery(
        createJob({
          project_id: projectId,
          name,
          source,
          command: command.trim() ? command.split(' ') : null,
          env,
          schedule: schedule.trim() || null,
          max_retries: maxRetries,
        }),
      )
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['jobs', projectId] })
      setName('')
      setImage('')
      setRegistryId('')
      setImageRef('')
      setImageMode('direct')
      setCommand('')
      setEnvStr('')
      setSchedule('')
      setMaxRetries(0)
      setCreateError('')
    },
    onError: (err: unknown) => {
      setCreateError(errorMessage(err) || 'Failed to create job')
    },
  })

  const sourceValid =
    imageMode === 'registry'
      ? registryId.trim().length > 0 && imageRef.trim().length > 0
      : image.trim().length > 0

  const handleCreate = (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim() || !sourceValid) return
    setCreateError('')
    create.mutate()
  }

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">scheduled work</p>
          <h1 className="t-display">Jobs</h1>
        </div>
        {projectId && jobs.length > 0 ? (
          <span className="badge">
            <Num>{jobs.length}</Num> {jobs.length === 1 ? 'job' : 'jobs'}
          </span>
        ) : null}
      </header>

      {!projectId ? (
        <div className="panel">
          <EmptyState
            icon={<ListChecks size={22} />}
            title="No project selected"
            hint="Pick a project to view and schedule its jobs."
            action={
              <Link to="/projects" className="btn btn-primary">
                Browse projects
              </Link>
            }
          />
        </div>
      ) : (
        <div className="stack-lg">
          {canOperate ? (
            <section>
              <p className="kicker" style={{ marginBottom: '0.9rem' }}>
                new job
              </p>
              <form onSubmit={handleCreate} className="panel panel-pad stack">
                <div className="cluster" style={{ alignItems: 'flex-end' }}>
                  <div className="min-w-0 flex-1">
                    <label className="kicker" htmlFor="job-name">
                      name
                    </label>
                    <input
                      id="job-name"
                      placeholder="Job name"
                      type="text"
                      value={name}
                      onChange={(e) => setName(e.target.value)}
                      className="field-input mt-1 w-full"
                      required
                    />
                  </div>
                  {imageMode === 'registry' ? (
                    <>
                      <div className="min-w-0">
                        <label className="kicker" htmlFor="job-registry">
                          registry
                        </label>
                        <select
                          id="job-registry"
                          aria-label="registry"
                          value={registryId}
                          onChange={(e) => setRegistryId(e.target.value)}
                          className="field-input mt-1 w-full"
                        >
                          <option value="">select registry</option>
                          {registries.map((r) => (
                            <option key={r.id} value={r.id}>
                              {r.name}
                            </option>
                          ))}
                        </select>
                      </div>
                      <div className="min-w-0 flex-1">
                        <label className="kicker" htmlFor="job-image-ref">
                          image ref
                        </label>
                        <input
                          id="job-image-ref"
                          placeholder="org/app:latest"
                          aria-label="image ref"
                          type="text"
                          value={imageRef}
                          onChange={(e) => setImageRef(e.target.value)}
                          className="field-input mt-1 w-full"
                        />
                      </div>
                    </>
                  ) : (
                    <div className="min-w-0 flex-1">
                      <label className="kicker" htmlFor="job-image">
                        image
                      </label>
                      <input
                        id="job-image"
                        placeholder="alpine:latest"
                        aria-label="image"
                        type="text"
                        value={image}
                        onChange={(e) => setImage(e.target.value)}
                        className="field-input mt-1 w-full"
                      />
                    </div>
                  )}
                  <button
                    type="submit"
                    className="btn btn-primary"
                    disabled={create.isPending || !sourceValid || !name.trim()}
                  >
                    {create.isPending ? (
                      <>
                        <span className="spin" aria-hidden="true" /> Creating
                      </>
                    ) : (
                      'Create job'
                    )}
                  </button>
                </div>

                {registries.length > 0 ? (
                  <div className="cluster">
                    <span className="kicker">image source</span>
                    <div className="segmented" role="group" aria-label="image source">
                      <button
                        type="button"
                        aria-pressed={imageMode === 'direct'}
                        onClick={() => setImageMode('direct')}
                      >
                        direct
                      </button>
                      <button
                        type="button"
                        aria-pressed={imageMode === 'registry'}
                        onClick={() => setImageMode('registry')}
                      >
                        registry + ref
                      </button>
                    </div>
                  </div>
                ) : null}

                <div className="cluster" style={{ alignItems: 'flex-end' }}>
                  <div className="min-w-0 flex-1">
                    <label className="kicker" htmlFor="job-command">
                      command
                    </label>
                    <input
                      id="job-command"
                      placeholder="Args (space-separated)"
                      type="text"
                      value={command}
                      onChange={(e) => setCommand(e.target.value)}
                      className="field-input mt-1 w-full"
                    />
                  </div>
                  <div className="min-w-0 flex-1">
                    <label className="kicker" htmlFor="job-schedule">
                      schedule
                    </label>
                    <input
                      id="job-schedule"
                      placeholder="Cron, e.g. */5 * * * *"
                      type="text"
                      value={schedule}
                      onChange={(e) => setSchedule(e.target.value)}
                      className="field-input mt-1 w-full"
                    />
                  </div>
                  <div className="w-24">
                    <label className="kicker" htmlFor="job-retries">
                      retries
                    </label>
                    <input
                      id="job-retries"
                      placeholder="0"
                      type="number"
                      value={maxRetries}
                      onChange={(e) =>
                        setMaxRetries(parseInt(e.target.value) || 0)
                      }
                      className="field-input mt-1 w-full"
                    />
                  </div>
                </div>

                <div>
                  <label className="kicker" htmlFor="job-env">
                    environment
                  </label>
                  <textarea
                    id="job-env"
                    placeholder="KEY=val, one per line"
                    value={envStr}
                    onChange={(e) => setEnvStr(e.target.value)}
                    rows={3}
                    className="field-input mt-1 w-full"
                    aria-describedby="job-env-help"
                  />
                  <p id="job-env-help" className="field-help">
                    Secrets: use credential references, not raw values.
                  </p>
                </div>

                {createError ? (
                  <p className="field-error" role="alert">
                    {createError}
                  </p>
                ) : null}
              </form>
            </section>
          ) : null}

          <section>
            <p className="kicker" style={{ marginBottom: '0.9rem' }}>
              all jobs
            </p>
            {isError ? (
              <ErrorPanel
                title="Failed to load jobs"
                message={errorMessage(error)}
              />
            ) : isLoading ? (
              <SkeletonRows rows={4} />
            ) : jobs.length === 0 ? (
              <div className="panel">
                <EmptyState
                  icon={<ListChecks size={22} />}
                  title="No jobs yet"
                  hint={
                    canOperate
                      ? 'Create your first job above to run one-off or scheduled work.'
                      : 'An operator can create jobs for this project.'
                  }
                />
              </div>
            ) : (
              <div className="panel overflow-hidden">
                <table className="dtable">
                  <thead>
                    <tr>
                      <th>job</th>
                      <th>source</th>
                      <th>schedule</th>
                      <th>next run</th>
                    </tr>
                  </thead>
                  <tbody>
                    {jobs.map((j) => {
                      const hint = j.schedule ? cronHint(j.schedule) : null
                      return (
                        <tr key={j.id}>
                          <td>
                            <Link to="/jobs/$jobId" params={{ jobId: j.id }}>
                              {j.name}
                            </Link>
                          </td>
                          <td className="tnum text-faint">
                            {sourceDisplay(j.source)}
                          </td>
                          <td>
                            {j.schedule ? (
                              <span className="cluster" style={{ gap: '0.4rem' }}>
                                <Timer
                                  size={13}
                                  aria-hidden="true"
                                  style={{ color: 'var(--fg-faint)' }}
                                />
                                <code className="tnum">{j.schedule}</code>
                                {hint ? (
                                  <span className="text-faint">{hint}</span>
                                ) : null}
                              </span>
                            ) : (
                              <span className="text-faint">manual</span>
                            )}
                          </td>
                          <td className="tnum text-faint">
                            {j.next_run_at
                              ? formatRelative(j.next_run_at, Date.now())
                              : '—'}
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
                {isFetching ? (
                  <p
                    className="kicker"
                    style={{ padding: '0.5rem 0.75rem', color: 'var(--fg-faint)' }}
                  >
                    refreshing…
                  </p>
                ) : null}
              </div>
            )}
          </section>
        </div>
      )}
    </div>
  )
}
