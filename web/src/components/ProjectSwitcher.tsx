import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useActiveProject } from '#/hooks/useActiveProject'

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

export function ProjectSwitcher() {
  const [activeProject, setActiveProject] = useActiveProject()

  const { data: projects = [] } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  if (projects.length === 0) return null

  const selected = projects.some((p) => p.id === activeProject)
    ? activeProject
    : ''

  return (
    <select
      value={selected}
      onChange={(e) => setActiveProject(e.target.value)}
      aria-label="Active project (scopes dashboard, services, jobs and registries)"
      className="btn text-xs py-2 px-3 min-w-0 max-w-[40vw] truncate sm:max-w-[14rem]"
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
