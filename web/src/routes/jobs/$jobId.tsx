import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { RunStatusSignal } from '#/components/RunStatusSignal'
import { useAuth, can } from '#/hooks/useAuth'

const getJob = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getJob(id)
  })

const runJob = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.runJob(id)
  })

const deleteJob = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteJob(id)
  })

const listJobRuns = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listJobRuns(id)
  })

function cronHint(cron: string): string | null {
  if (cron === '*/1 * * * *') return 'every minute'
  if (cron === '*/5 * * * *') return 'every 5 min'
  if (cron === '*/15 * * * *') return 'every 15 min'
  if (cron === '*/30 * * * *') return 'every 30 min'
  return null
}

function sourceDisplay(source: unknown): string {
  const s = source as Record<string, unknown>
  if (s.type === 'external_image') return String(s.image)
  if (s.type === 'git') return String(s.repo_url)
  return JSON.stringify(source)
}

function isActive(status: string): boolean {
  return status === 'Pending' || status === 'Running'
}

export const Route = createFileRoute('/jobs/$jobId')({
  component: JobDetail,
})

export function JobDetail() {
  const queryClient = useQueryClient()
  const auth = useAuth()
  const { jobId } = Route.useParams()

  const { data: job, isLoading, isError, error } = useQuery({
    queryKey: ['jobs', jobId],
    queryFn: () => runQuery(getJob(jobId)),
  })

  const { data: runs = [] } = useQuery({
    queryKey: ['jobs', jobId, 'runs'],
    queryFn: () => runQuery(listJobRuns(jobId)),
    refetchInterval: () => {
      const newest = runs[0]
      if (newest && isActive(newest.status)) return 2000
      return false
    },
  })

  const userRole = job
    ? auth.roleForActiveProject(Number(job.project_id))
    : undefined
  const canOperate = userRole ? can('operator', userRole) : false

  const runMutation = useMutation({
    mutationFn: () => runQuery(runJob(jobId)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['jobs', jobId, 'runs'] })
      if (job) {
        queryClient.invalidateQueries({ queryKey: ['jobs', job.project_id] })
      }
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () => runQuery(deleteJob(jobId)),
    onSuccess: () => {
      if (job) {
        queryClient.invalidateQueries({ queryKey: ['jobs', job.project_id] })
      }
    },
  })

  if (isLoading) {
    return (
      <main className="page-wrap px-4 pb-12 pt-12">
        <p className="text-[var(--fg-muted)]">loading...</p>
      </main>
    )
  }

  if (isError || !job) {
    return (
      <main className="page-wrap px-4 pb-12 pt-12">
        <p className="text-sm signal-fault">
          {error instanceof Error ? error.message : 'Job not found'}
        </p>
        <a href="/jobs" className="btn mt-4 text-xs">
          &larr; back to jobs
        </a>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">
        job{' '}
        <a href="/jobs" className="text-[var(--fg-muted)]">
          &larr; back
        </a>
      </p>
      <div className="mb-6 flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold tracking-tight text-[var(--fg)]">
          {job.name}
        </h1>
        {canOperate && (
          <>
            <button
              className="btn btn-primary text-xs"
              type="button"
              onClick={() => runMutation.mutate()}
              disabled={runMutation.isPending}
            >
              {runMutation.isPending ? 'running...' : 'run now'}
            </button>
            <button
              className="btn text-xs"
              type="button"
              onClick={() => {
                if (confirm('Delete this job and all its runs?')) {
                  deleteMutation.mutate()
                }
              }}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? 'deleting...' : 'delete'}
            </button>
          </>
        )}
      </div>

      {runMutation.isError && (
        <p className="text-sm signal-fault mb-4">
          {(runMutation.error as Error)?.message ?? 'Run failed'}
        </p>
      )}

      {deleteMutation.isError && (
        <p className="text-sm signal-fault mb-4">
          {(deleteMutation.error as Error)?.message ?? 'Delete failed'}
        </p>
      )}

      <section className="panel mb-8 p-4 space-y-2">
        <p className="kicker m-0">schedule</p>
        <div className="flex flex-wrap gap-x-6 gap-y-1 text-sm">
          <span>
            <span className="text-[var(--fg-muted)]">source:</span>{' '}
            {sourceDisplay(job.source)}
          </span>
          {job.command && (
            <span>
              <span className="text-[var(--fg-muted)]">command:</span>{' '}
              <code>{job.command.join(' ')}</code>
            </span>
          )}
          <span>
            <span className="text-[var(--fg-muted)]">schedule:</span>{' '}
            {job.schedule || 'manual only'}
            {job.schedule && cronHint(job.schedule) && (
              <span className="ml-1 opacity-60">
                ({cronHint(job.schedule)})
              </span>
            )}
          </span>
          {job.next_run_at && (
            <span className="tnum">
              <span className="text-[var(--fg-muted)]">next run:</span>{' '}
              {new Date(job.next_run_at).toLocaleString()}
            </span>
          )}
          <span className="tnum">
            <span className="text-[var(--fg-muted)]">max retries:</span>{' '}
            {job.max_retries}
          </span>
        </div>
      </section>

      {job.env.length > 0 && (
        <section className="panel mb-8 overflow-hidden">
          <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
            environment
          </p>
          <ul className="m-0 list-none">
            {job.env.map(([k, v], i) => (
              <li
                key={k}
                className={`flex gap-4 px-4 py-1.5 text-xs font-mono ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span className="text-[var(--fg)]">{k}=</span>
                <span className="text-[var(--fg-muted)]">{v}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="mb-8">
        <p className="kicker mb-2">
          run history{' '}
          <span className="text-[var(--fg-muted)]">{runs.length}</span>
        </p>
        {runs.length === 0 ? (
          <p className="text-sm text-[var(--fg-muted)]">No runs yet.</p>
        ) : (
          <div className="panel overflow-hidden">
            <ul className="m-0 list-none">
              {runs.map((run, i) => (
                <li
                  key={run.id}
                  className={`flex items-center gap-4 px-4 py-3 text-sm ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  <RunStatusSignal status={run.status} />
                  <span className="tnum text-xs text-[var(--fg-muted)]">
                    #{run.attempt}
                  </span>
                  {run.exit_code !== null && (
                    <span
                      className={`tnum text-xs ${
                        run.exit_code === 0
                          ? 'text-[var(--ok)]'
                          : 'text-[var(--violet)]'
                      }`}
                    >
                      exit {run.exit_code}
                    </span>
                  )}
                  {run.started_at && (
                    <span className="tnum text-xs text-[var(--fg-muted)]">
                      {new Date(run.started_at).toLocaleString()}
                    </span>
                  )}
                  {run.finished_at && (
                    <span className="tnum text-xs text-[var(--fg-muted)]">
                      &rarr; {new Date(run.finished_at).toLocaleString()}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>
    </main>
  )
}
