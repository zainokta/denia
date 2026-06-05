import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ChevronDown, ChevronRight, Play, Timer } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { JobRun, ServiceSource } from '#/effect/schema'
import { useAuth, can } from '#/hooks/useAuth'
import { StatusBadge } from '#/components/StatusBadge'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { ConfirmButton } from '#/components/ConfirmButton'
import { useActionToasts } from '#/components/Toast'
import { Num } from '#/components/Num'
import { formatDateTime, formatDuration, formatRelative } from '#/lib/format'

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

function sourceDisplay(source: ServiceSource): string {
  switch (source.type) {
    case 'external_image':
      return source.image_ref ?? source.image
    case 'git':
      return source.repo_url
    case 'upload':
      return 'upload (denia push)'
  }
}

function isActive(status: string): boolean {
  return status === 'Pending' || status === 'Running'
}

// Run duration in seconds when both endpoints are known, else null.
function runSeconds(run: JobRun): number | null {
  if (!run.started_at || !run.finished_at) return null
  const start = Date.parse(run.started_at)
  const end = Date.parse(run.finished_at)
  if (Number.isNaN(start) || Number.isNaN(end)) return null
  return Math.max(0, (end - start) / 1000)
}

export const Route = createFileRoute('/jobs/$jobId')({
  component: JobDetail,
})

export function JobDetail() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const auth = useAuth()
  const toast = useActionToasts()
  const { jobId } = Route.useParams()
  const [expanded, setExpanded] = useState<string | null>(null)

  const { data: job, isLoading, isError, error } = useQuery({
    queryKey: ['jobs', jobId],
    queryFn: () => runQuery(getJob(jobId)),
  })

  const { data: runs = [], isLoading: runsLoading } = useQuery({
    queryKey: ['jobs', jobId, 'runs'],
    queryFn: () => runQuery(listJobRuns(jobId)),
    refetchInterval: () => {
      const newest = runs[0]
      if (newest && isActive(newest.status)) return 2000
      return false
    },
  })

  const userRole = job ? auth.roleForActiveProject(job.project_id) : undefined
  const canOperate = userRole ? can('operator', userRole) : false

  const runMutation = useMutation({
    mutationFn: () => runQuery(runJob(jobId)),
    onSuccess: () => {
      toast.ok('Run enqueued.')
      queryClient.invalidateQueries({ queryKey: ['jobs', jobId, 'runs'] })
      if (job) {
        queryClient.invalidateQueries({ queryKey: ['jobs', job.project_id] })
      }
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  const deleteMutation = useMutation({
    mutationFn: () => runQuery(deleteJob(jobId)),
    onSuccess: () => {
      toast.ok('Job deleted.')
      if (job) {
        queryClient.invalidateQueries({ queryKey: ['jobs', job.project_id] })
      }
      navigate({ to: '/jobs' })
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
  })

  if (isLoading) {
    return (
      <div className="page-wrap px-4 pb-16 pt-10">
        <p className="kicker mb-3">
          <Link to="/jobs" className="text-faint">
            &larr; jobs
          </Link>
        </p>
        <SkeletonRows rows={4} />
      </div>
    )
  }

  if (isError || !job) {
    return (
      <div className="page-wrap px-4 pb-16 pt-10">
        <p className="kicker mb-3">
          <Link to="/jobs" className="text-faint">
            &larr; jobs
          </Link>
        </p>
        <ErrorPanel
          title="Job not found"
          message={isError ? errorMessage(error) : 'This job no longer exists.'}
        />
      </div>
    )
  }

  const hint = job.schedule ? cronHint(job.schedule) : null

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <p className="kicker mb-3">
        <Link to="/jobs" className="text-faint">
          &larr; jobs
        </Link>
      </p>

      <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
        <div>
          <p className="kicker">job</p>
          <h1 className="t-display">{job.name}</h1>
        </div>
        {canOperate ? (
          <div className="cluster">
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => runMutation.mutate()}
              disabled={runMutation.isPending}
            >
              {runMutation.isPending ? (
                <span className="spin" aria-hidden="true" />
              ) : (
                <Play size={14} aria-hidden="true" />
              )}
              Run now
            </button>
            <ConfirmButton
              label="Delete"
              confirmLabel="Delete job"
              message="Delete this job and all of its runs? This cannot be undone."
              onConfirm={() => deleteMutation.mutate()}
              busy={deleteMutation.isPending}
              align="right"
            />
          </div>
        ) : null}
      </header>

      <div className="stack-lg">
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            configuration
          </p>
          <div className="panel panel-pad">
            <dl className="flex flex-col gap-3" style={{ margin: 0 }}>
              <Detail label="source" value={sourceDisplay(job.source)} mono />
              <Detail
                label="command"
                value={job.command ? job.command.join(' ') : 'image default'}
                mono
              />
              <Detail
                label="schedule"
                value={
                  job.schedule ? (
                    <span className="cluster" style={{ gap: '0.4rem' }}>
                      <Timer
                        size={13}
                        aria-hidden="true"
                        style={{ color: 'var(--fg-faint)' }}
                      />
                      <code className="tnum">{job.schedule}</code>
                      {hint ? <span className="text-faint">{hint}</span> : null}
                    </span>
                  ) : (
                    <span className="text-faint">manual only</span>
                  )
                }
              />
              <Detail
                label="max retries"
                value={<Num>{job.max_retries}</Num>}
              />
              {job.next_run_at ? (
                <Detail
                  label="next run"
                  value={
                    <Num title={formatDateTime(job.next_run_at)}>
                      {formatRelative(job.next_run_at, Date.now())}
                    </Num>
                  }
                />
              ) : null}
              <Detail
                label="created"
                value={
                  <Num title={formatDateTime(job.created_at)}>
                    {formatRelative(job.created_at, Date.now())}
                  </Num>
                }
              />
            </dl>
            <p className="field-help" style={{ marginTop: '0.9rem' }}>
              Jobs are immutable; recreate to change configuration.
            </p>
          </div>
        </section>

        {job.env.length > 0 ? (
          <section>
            <p className="kicker" style={{ marginBottom: '0.9rem' }}>
              environment
            </p>
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>key</th>
                    <th>value</th>
                  </tr>
                </thead>
                <tbody>
                  {job.env.map(([k, v]) => (
                    <tr key={k}>
                      <td>
                        <code className="tnum">{k}</code>
                      </td>
                      <td className="text-faint">
                        <code className="tnum">{v}</code>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
        ) : null}

        <section>
          <div className="panel-head">
            <p className="kicker">run history</p>
            {runs.length > 0 ? (
              <span className="badge">
                <Num>{runs.length}</Num> {runs.length === 1 ? 'run' : 'runs'}
              </span>
            ) : null}
          </div>
          {runsLoading ? (
            <SkeletonRows rows={3} />
          ) : runs.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Play size={20} />}
                title="No runs yet"
                hint={
                  canOperate
                    ? 'Trigger a run with "Run now", or wait for the schedule to fire.'
                    : 'Runs will appear here once this job executes.'
                }
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th style={{ width: '2rem' }} aria-label="expand" />
                    <th className="num">attempt</th>
                    <th>status</th>
                    <th>started</th>
                    <th>duration</th>
                    <th className="num">exit</th>
                  </tr>
                </thead>
                <tbody>
                  {runs.map((run) => {
                    const secs = runSeconds(run)
                    const isOpen = expanded === run.id
                    return (
                      <RunRow
                        key={run.id}
                        run={run}
                        seconds={secs}
                        open={isOpen}
                        onToggle={() =>
                          setExpanded((cur) => (cur === run.id ? null : run.id))
                        }
                      />
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </div>
  )
}

// One run row plus an expandable detail panel. The backend exposes no per-run
// log stream, so the disclosure surfaces the full run record (timestamps,
// attempt, exit status) instead of raw output.
function RunRow({
  run,
  seconds,
  open,
  onToggle,
}: {
  readonly run: JobRun
  readonly seconds: number | null
  readonly open: boolean
  readonly onToggle: () => void
}) {
  return (
    <>
      <tr>
        <td>
          <button
            type="button"
            className="btn-icon"
            aria-expanded={open}
            aria-label={open ? 'Collapse run detail' : 'Expand run detail'}
            onClick={onToggle}
            style={{
              border: 0,
              background: 'transparent',
              color: 'var(--fg-muted)',
              padding: 0,
            }}
          >
            {open ? (
              <ChevronDown size={15} aria-hidden="true" />
            ) : (
              <ChevronRight size={15} aria-hidden="true" />
            )}
          </button>
        </td>
        <td className="num">
          <Num>{run.attempt}</Num>
        </td>
        <td>
          <StatusBadge status={run.status} kind="run" />
        </td>
        <td className="tnum text-faint">
          {run.started_at ? (
            <span title={formatDateTime(run.started_at)}>
              {formatRelative(run.started_at, Date.now())}
            </span>
          ) : (
            '—'
          )}
        </td>
        <td className="tnum text-faint">
          {seconds !== null ? formatDuration(seconds) : '—'}
        </td>
        <td className="num">
          {run.exit_code !== null ? <Num>{run.exit_code}</Num> : '—'}
        </td>
      </tr>
      {open ? (
        <tr>
          <td colSpan={6} style={{ background: 'var(--surface-2)' }}>
            <dl
              className="flex flex-col gap-2"
              style={{ margin: 0, padding: '0.4rem 0' }}
            >
              <Detail
                label="status"
                value={<StatusBadge status={run.status} kind="run" />}
              />
              <Detail label="attempt" value={<Num>{run.attempt}</Num>} />
              <Detail
                label="exit code"
                value={run.exit_code !== null ? <Num>{run.exit_code}</Num> : '—'}
              />
              <Detail
                label="started"
                value={
                  run.started_at ? (
                    <Num>{formatDateTime(run.started_at)}</Num>
                  ) : (
                    '—'
                  )
                }
              />
              <Detail
                label="finished"
                value={
                  run.finished_at ? (
                    <Num>{formatDateTime(run.finished_at)}</Num>
                  ) : (
                    '—'
                  )
                }
              />
              <Detail
                label="duration"
                value={seconds !== null ? formatDuration(seconds) : '—'}
              />
              <Detail
                label="enqueued"
                value={<Num>{formatDateTime(run.created_at)}</Num>}
              />
            </dl>
          </td>
        </tr>
      ) : null}
    </>
  )
}

function Detail({
  label,
  value,
  mono = false,
}: {
  readonly label: string
  readonly value: React.ReactNode
  readonly mono?: boolean
}) {
  return (
    <div className="flex items-baseline gap-3">
      <dt className="kicker" style={{ minWidth: '9ch', flexShrink: 0 }}>
        {label}
      </dt>
      <dd
        className={mono ? 'tnum' : undefined}
        style={{ margin: 0, minWidth: 0, wordBreak: 'break-word' }}
      >
        {value}
      </dd>
    </div>
  )
}
