import { createFileRoute, Link } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth, can } from '#/hooks/useAuth'
import { useActiveProject } from '#/hooks/useActiveProject'

const listJobs = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listJobs(projectId)
  })

const createJob = (input: {
  id: string
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
  next_run_at: null
  last_enqueued_at: null
  created_at: string
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

  const { data: jobs = [], isFetching } = useQuery({
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
          id: crypto.randomUUID(),
          project_id: projectId,
          name,
          source,
          command: command.trim() ? command.split(' ') : null,
          env,
          schedule: schedule.trim() || null,
          max_retries: maxRetries,
          next_run_at: null,
          last_enqueued_at: null,
          created_at: new Date().toISOString(),
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
    onError: (error: unknown) => {
      setCreateError(
        error instanceof Error ? error.message : 'Failed to create job',
      )
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
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">scheduled work</p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Jobs
      </h1>

      {!projectId ? (
        <section className="panel p-8 text-center">
          <p className="text-[var(--fg-muted)] mb-4">
            Select a project to view its jobs.
          </p>
          <a href="/projects" className="btn btn-primary text-xs">
            Browse Projects
          </a>
        </section>
      ) : (
        <>
          {canOperate && (
            <form onSubmit={handleCreate} className="panel mb-8 p-4 space-y-3">
              <p className="kicker m-0">new job</p>
              <div className="flex flex-wrap gap-3 items-end">
                <div className="flex-1 min-w-0">
                  <input
                    placeholder="Job name"
                    type="text"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                    required
                  />
                </div>
                {imageMode === 'registry' ? (
                  <>
                    <div className="min-w-0">
                      <select
                        aria-label="registry"
                        value={registryId}
                        onChange={(e) => setRegistryId(e.target.value)}
                        className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                      >
                        <option value="">select registry</option>
                        {registries.map((r) => (
                          <option key={r.id} value={r.id}>
                            {r.name}
                          </option>
                        ))}
                      </select>
                    </div>
                    <div className="flex-1 min-w-0">
                      <input
                        placeholder="Image ref (e.g. org/app:latest)"
                        aria-label="image ref"
                        type="text"
                        value={imageRef}
                        onChange={(e) => setImageRef(e.target.value)}
                        className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                      />
                    </div>
                  </>
                ) : (
                  <div className="flex-1 min-w-0">
                    <input
                      placeholder="Image (e.g. alpine:latest)"
                      aria-label="image"
                      type="text"
                      value={image}
                      onChange={(e) => setImage(e.target.value)}
                      className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                    />
                  </div>
                )}
                <button
                  type="submit"
                  className="btn btn-primary"
                  disabled={create.isPending || !sourceValid || !name.trim()}
                >
                  {create.isPending ? 'creating...' : 'create'}
                </button>
              </div>

              {registries.length > 0 ? (
                <div className="flex items-center gap-2 text-xs">
                  <span className="kicker m-0">image source</span>
                  <button
                    type="button"
                    aria-pressed={imageMode === 'direct'}
                    className={`btn text-xs ${imageMode === 'direct' ? 'btn-primary' : ''}`}
                    onClick={() => setImageMode('direct')}
                  >
                    direct
                  </button>
                  <button
                    type="button"
                    aria-pressed={imageMode === 'registry'}
                    className={`btn text-xs ${imageMode === 'registry' ? 'btn-primary' : ''}`}
                    onClick={() => setImageMode('registry')}
                  >
                    registry + ref
                  </button>
                </div>
              ) : null}
              <div className="flex flex-wrap gap-3">
                <div className="flex-1 min-w-0">
                  <input
                    placeholder="Command args (space-separated)"
                    type="text"
                    value={command}
                    onChange={(e) => setCommand(e.target.value)}
                    className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                  />
                </div>
                <div className="flex-1 min-w-0">
                  <input
                    placeholder="Cron schedule (e.g. */5 * * * *)"
                    type="text"
                    value={schedule}
                    onChange={(e) => setSchedule(e.target.value)}
                    className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                  />
                </div>
                <div className="w-20">
                  <input
                    placeholder="Retries"
                    type="number"
                    value={maxRetries}
                    onChange={(e) => setMaxRetries(parseInt(e.target.value) || 0)}
                    className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
                  />
                </div>
              </div>
              <div>
                <textarea
                  placeholder="Env vars (KEY=val, one per line). Secrets: use credential references, not raw values."
                  value={envStr}
                  onChange={(e) => setEnvStr(e.target.value)}
                  rows={3}
                  className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] font-mono focus:outline-none focus:border-[var(--pink)]"
                />
              </div>
              {createError && (
                <p className="text-sm signal-fault">{createError}</p>
              )}
            </form>
          )}

          {jobs.length === 0 && !isFetching ? (
            <p className="text-[var(--fg-muted)]">
              No jobs yet. {canOperate ? 'Create your first job above.' : ''}
            </p>
          ) : (
            <section className="panel overflow-hidden">
              <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
                <p className="kicker m-0">
                  {isFetching
                    ? 'fetching...'
                    : `${jobs.length} job${jobs.length !== 1 ? 's' : ''}`}
                </p>
              </div>
              <ul className="m-0 list-none">
                {jobs.map((j, i) => (
                  <li
                    key={j.id}
                    className={`flex items-center gap-4 px-4 py-3 text-sm ${
                      i > 0 ? 'border-t border-[var(--border)]' : ''
                    }`}
                  >
                    <span className="signal" aria-hidden="true" />
                    <Link
                      to="/jobs/$jobId"
                      params={{ jobId: j.id }}
                      className="min-w-0 flex-1 text-[var(--fg)] no-underline hover:underline"
                    >
                      <span className="font-semibold">{j.name}</span>
                      <span className="ml-3 text-xs text-[var(--fg-muted)]">
                        {sourceDisplay(j.source)}
                      </span>
                    </Link>
                    {j.schedule && (
                      <span className="text-xs text-[var(--fg-muted)]">
                        {j.schedule}
                        {cronHint(j.schedule) && (
                          <span className="ml-1 opacity-60">
                            ({cronHint(j.schedule)})
                          </span>
                        )}
                      </span>
                    )}
                    {j.next_run_at && (
                      <span className="tnum text-xs text-[var(--fg-muted)]">
                        next {new Date(j.next_run_at).toLocaleString()}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </main>
  )
}
