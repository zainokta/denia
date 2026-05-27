import { createFileRoute, Link } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useActiveProject } from '#/hooks/useActiveProject'

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

const createProject = (input: {
  name: string
  description: string | null
  shared_env: Array<{ key: string; value: string }>
  default_resource_limits: {
    cpu_millis: number
    memory_bytes: number
  } | null
}) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createProject(input)
  })

export const Route = createFileRoute('/projects/')({
  component: ProjectsIndex,
})

export function ProjectsIndex() {
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [createError, setCreateError] = useState('')
  const [, setActiveProject] = useActiveProject()

  const { data: projects = [], isFetching } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  const create = useMutation({
    mutationFn: () =>
      runQuery(
        createProject({
          name,
          description: description || null,
          shared_env: [],
          default_resource_limits: null,
        }),
      ),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      setName('')
      setDescription('')
      setCreateError('')
    },
    onError: (error: unknown) => {
      setCreateError(
        error instanceof Error ? error.message : 'Failed to create project',
      )
    },
  })

  const handleCreate = (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return
    setCreateError('')
    create.mutate()
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">projects</p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Projects
      </h1>

      <form onSubmit={handleCreate} className="panel mb-8 p-4 space-y-3">
        <h2 className="kicker m-0">new project</h2>
        <div className="flex flex-wrap gap-3 items-end">
          <div className="flex-1 min-w-0">
            <label htmlFor="new-project-name" className="sr-only">
              Project name
            </label>
            <input
              id="new-project-name"
              placeholder="Project name"
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              aria-describedby={createError ? 'new-project-error' : undefined}
              className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:border-[var(--pink)]"
              required
            />
          </div>
          <div className="flex-1 min-w-0">
            <label htmlFor="new-project-description" className="sr-only">
              Description (optional)
            </label>
            <input
              id="new-project-description"
              placeholder="Description (optional)"
              type="text"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:border-[var(--pink)]"
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={create.isPending}
          >
            {create.isPending ? 'creating...' : 'create'}
          </button>
        </div>
        {createError && (
          <p id="new-project-error" role="alert" className="text-sm signal-fault">
            {createError}
          </p>
        )}
      </form>

      {projects.length === 0 && !isFetching ? (
        <p className="text-[var(--fg-muted)]">
          No projects yet. Create your first project above.
        </p>
      ) : (
        <section className="panel overflow-hidden">
          <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
            <p className="kicker m-0">
              {isFetching
                ? 'fetching...'
                : `${projects.length} project${projects.length !== 1 ? 's' : ''}`}
            </p>
          </div>
          <ul className="m-0 list-none">
            {projects.map((p, i) => (
              <li
                key={p.id}
                className={`flex items-center gap-4 px-4 py-3 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span className="signal signal-steady" />
                <Link
                  to="/projects/$projectId"
                  params={{ projectId: p.id }}
                  onClick={() => setActiveProject(p.id)}
                  className="min-w-0 flex-1 text-[var(--fg)] no-underline hover:underline"
                >
                  <span className="font-semibold">{p.name}</span>
                  {p.description && (
                    <span className="ml-3 text-xs text-[var(--fg-muted)]">
                      {p.description}
                    </span>
                  )}
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}
    </main>
  )
}
