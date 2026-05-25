import { useQuery } from '@tanstack/react-query'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

export function ProjectSwitcher() {
  const navigate = useNavigate()
  const search = useSearch({ strict: false }) as { project?: string }
  const activeProject = search.project ?? ''

  const { data: projects = [] } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  const handleChange = (projectId: string) => {
    navigate({
      search: (prev: Record<string, string>) => ({
        ...prev,
        project: projectId || undefined,
      }),
    })
  }

  if (projects.length === 0) return null

  return (
    <select
      value={activeProject}
      onChange={(e) => handleChange(e.target.value)}
      className="btn text-xs py-2 px-3 min-w-0"
    >
      <option value="">all projects</option>
      {projects.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name}
        </option>
      ))}
    </select>
  )
}
