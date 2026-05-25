import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'

const getProject = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.getProject(id)
  })

const deleteProject = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteProject(id)
  })

export const Route = createFileRoute('/projects/$projectId')({
  component: ProjectDetail,
})

export function ProjectDetail() {
  const { projectId } = Route.useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [deleteError, setDeleteError] = useState('')

  const { data: project, isFetching } = useQuery({
    queryKey: ['projects', projectId],
    queryFn: () => runQuery(getProject(projectId)),
  })

  const del = useMutation({
    mutationFn: () => runQuery(deleteProject(projectId)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      navigate({ to: '/projects' })
    },
    onError: (error: unknown) => {
      const msg =
        error instanceof Error ? error.message : 'Failed to delete'
      setDeleteError(msg)
    },
  })

  if (isFetching && !project) {
    return (
      <main className="page-wrap px-4 pb-12 pt-12">
        <p className="kicker mb-3">projects</p>
        <p className="text-[var(--fg-muted)]">loading...</p>
      </main>
    )
  }

  if (!project) {
    return (
      <main className="page-wrap px-4 pb-12 pt-12">
        <p className="kicker mb-3">projects</p>
        <p className="text-[var(--fg-muted)]">Project not found.</p>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">
        <Link to="/projects" className="no-underline hover:underline">
          projects
        </Link>{' '}
        / {project.name}
      </p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        {project.name}
      </h1>

      {project.description && (
        <p className="mb-6 text-[var(--fg-muted)]">{project.description}</p>
      )}

      <section className="panel mb-8 overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          shared environment
        </p>
        {project.shared_env.length === 0 ? (
          <p className="px-4 py-3 text-sm text-[var(--fg-muted)]">
            No shared environment variables set.
          </p>
        ) : (
          <dl className="m-0">
            {project.shared_env.map(({ key, value }, i) => (
              <div
                key={key}
                className={`flex items-baseline gap-4 px-4 py-3 ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <dt className="w-48 flex-shrink-0 text-sm font-semibold text-[var(--fg)]">
                  {key}
                </dt>
                <dd className="m-0 text-sm text-[var(--fg-muted)] truncate">
                  {value}
                </dd>
              </div>
            ))}
          </dl>
        )}
      </section>

      {project.default_resource_limits && (
        <section className="panel mb-8 overflow-hidden">
          <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
            default resource limits
          </p>
          <dl className="m-0">
            <div className="flex items-baseline gap-4 px-4 py-3">
              <dt className="w-48 flex-shrink-0 text-sm font-semibold text-[var(--fg)]">
                CPU
              </dt>
              <dd className="m-0 text-sm text-[var(--fg-muted)]">
                {project.default_resource_limits.cpu_millis} millis
              </dd>
            </div>
            <div className="flex items-baseline gap-4 border-t border-[var(--border)] px-4 py-3">
              <dt className="w-48 flex-shrink-0 text-sm font-semibold text-[var(--fg)]">
                Memory
              </dt>
              <dd className="m-0 text-sm text-[var(--fg-muted)]">
                {Math.round(
                  project.default_resource_limits.memory_bytes / 1048576,
                )}{' '}
                MB
              </dd>
            </div>
          </dl>
        </section>
      )}

      <section className="mb-8">
        {deleteError && (
          <div className="panel mb-4 p-4">
            <p className="m-0 flex items-center gap-2 text-sm signal-fault">
              <span className="signal signal-fault" />
              {deleteError}
            </p>
          </div>
        )}
        <button
          type="button"
          className="btn"
          onClick={() => {
            if (window.confirm(`Delete project "${project.name}"?`)) {
              setDeleteError('')
              del.mutate()
            }
          }}
          disabled={del.isPending}
        >
          {del.isPending ? 'deleting...' : 'delete project'}
        </button>
      </section>
    </main>
  )
}
