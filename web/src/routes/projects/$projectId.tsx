import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { Boxes, Trash2, Users } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useActiveProject } from '#/hooks/useActiveProject'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { ErrorPanel, InlineError, errorMessage } from '#/components/ErrorPanel'
import { ConfirmButton } from '#/components/ConfirmButton'
import { StatusBadge } from '#/components/StatusBadge'
import { useActionToasts } from '#/components/Toast'
import { Num } from '#/components/Num'
import { formatBytes } from '#/lib/format'
import type { Role } from '#/effect/schema'

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

const listMembers = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listMembers(projectId)
  })

const listUserDirectory = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listUserDirectory
})

const listServices = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listServices
})

const listWorkloads = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listWorkloads
})

const addMember = (projectId: string, userId: string, role: Role) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.addMember(projectId, userId, role)
  })

const removeMember = (projectId: string, userId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.removeMember(projectId, userId)
  })

export const Route = createFileRoute('/projects/$projectId')({
  component: ProjectDetail,
})

export function ProjectDetail() {
  const { projectId } = Route.useParams()
  const navigate = useNavigate()
  const [, setActiveProject] = useActiveProject()
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const { isSuperAdmin, roleForActiveProject } = useAuth()
  const [newUserId, setNewUserId] = useState<string>('')
  const [newRole, setNewRole] = useState<Role>('viewer')
  const [addMemberError, setAddMemberError] = useState('')

  const {
    data: project,
    isLoading,
    isError,
    error,
    refetch,
  } = useQuery({
    queryKey: ['projects', projectId],
    queryFn: () => runQuery(getProject(projectId)),
  })

  const canManage = isSuperAdmin || roleForActiveProject(projectId) === 'admin'

  const { data: members = [] } = useQuery({
    queryKey: ['projects', projectId, 'members'],
    queryFn: () => runQuery(listMembers(projectId)),
    enabled: projectId.length > 0,
  })

  const { data: userDirectory = [] } = useQuery({
    queryKey: ['users', 'directory'],
    queryFn: () => runQuery(listUserDirectory),
    enabled: canManage,
  })

  const { data: allServices = [] } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const { data: workloads = [] } = useQuery({
    queryKey: ['workloads'],
    queryFn: () => runQuery(listWorkloads),
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  })

  const projectServices = allServices.filter((s) => s.project_id === projectId)
  const statusByService = new Map(
    workloads.map((w) => [w.service_id, w.status]),
  )

  const usernameById = new Map(userDirectory.map((u) => [u.id, u.username]))
  const memberIds = new Set(members.map((m) => m.user_id))
  const availableUsers = userDirectory.filter((u) => !memberIds.has(u.id))

  const addMemberMutation = useMutation({
    mutationFn: (input: { userId: string; role: Role }) =>
      runQuery(addMember(projectId, input.userId, input.role)),
    onSuccess: () => {
      setNewUserId('')
      setAddMemberError('')
      toast.ok('Member added')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
    onError: (err: unknown) => {
      const msg = errorMessage(err) || 'Failed to add member'
      setAddMemberError(msg)
      toast.err(msg)
    },
  })

  const removeMemberMutation = useMutation({
    mutationFn: (userId: string) => runQuery(removeMember(projectId, userId)),
    onSuccess: () => {
      toast.ok('Member removed')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
    onError: (err: unknown) => {
      toast.err(errorMessage(err) || 'Failed to remove member')
    },
  })

  const del = useMutation({
    mutationFn: () => runQuery(deleteProject(projectId)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      toast.ok('Project deleted')
      navigate({ to: '/projects' })
    },
    onError: (err: unknown) => {
      toast.err(errorMessage(err) || 'Failed to delete project')
    },
  })

  if (isLoading && !project) {
    return (
      <main className="page-wrap px-4 pb-16 pt-10" aria-busy="true">
        <p className="kicker" style={{ marginBottom: '0.9rem' }}>
          projects
        </p>
        <SkeletonRows rows={4} />
      </main>
    )
  }

  if (isError && !project) {
    return (
      <main className="page-wrap px-4 pb-16 pt-10">
        <p className="kicker" style={{ marginBottom: '0.9rem' }}>
          projects
        </p>
        <ErrorPanel
          title="Could not load project"
          message={errorMessage(error)}
          onRetry={() => refetch()}
        />
      </main>
    )
  }

  if (!project) {
    return (
      <main className="page-wrap px-4 pb-16 pt-10">
        <p className="kicker" style={{ marginBottom: '0.9rem' }}>
          projects
        </p>
        <div className="panel">
          <EmptyState
            icon={<Boxes size={22} />}
            title="Project not found"
            hint="It may have been deleted, or the id is wrong."
            action={
              <Link to="/projects" className="btn">
                Back to projects
              </Link>
            }
          />
        </div>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <nav aria-label="Breadcrumb" style={{ marginBottom: '0.6rem' }}>
        <ol className="kicker m-0 flex list-none flex-wrap items-center gap-x-2 p-0">
          <li>
            <Link to="/projects">projects</Link>
          </li>
          <li aria-hidden="true">/</li>
          <li aria-current="page">{project.name}</li>
        </ol>
      </nav>

      <header className="panel-head">
        <div>
          <p className="kicker">project</p>
          <h1 className="t-display">{project.name}</h1>
        </div>
        {canManage ? (
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} aria-hidden="true" /> Delete project
              </>
            }
            confirmLabel="Delete project"
            message={`Delete "${project.name}"? This removes the project and cannot be undone.`}
            busy={del.isPending}
            onConfirm={() => del.mutate()}
            align="right"
          />
        ) : null}
      </header>

      {project.description ? (
        <p className="text-faint" style={{ marginBottom: '1.5rem', maxWidth: '70ch' }}>
          {project.description}
        </p>
      ) : null}

      <div className="stack-lg">
        {/* Overview: shared env + default limits (read-only) */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            overview
          </p>
          <div className="panel panel-pad stack">
            <div>
              <p className="kicker" style={{ marginBottom: '0.6rem' }}>
                shared environment
              </p>
              {project.shared_env.length === 0 ? (
                <p className="text-faint">No shared environment variables.</p>
              ) : (
                <table className="dtable">
                  <thead>
                    <tr>
                      <th>key</th>
                      <th>value</th>
                    </tr>
                  </thead>
                  <tbody>
                    {project.shared_env.map(({ key, value }) => (
                      <tr key={key}>
                        <td>{key}</td>
                        <td className="text-faint" style={{ wordBreak: 'break-word' }}>
                          {value}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div>
              <p className="kicker" style={{ marginBottom: '0.6rem' }}>
                default resource limits
              </p>
              {project.default_resource_limits ? (
                <table className="dtable">
                  <tbody>
                    <tr>
                      <td>cpu</td>
                      <td className="num">
                        <Num>{project.default_resource_limits.cpu_millis}</Num>{' '}
                        millis
                      </td>
                    </tr>
                    <tr>
                      <td>memory</td>
                      <td className="num">
                        <Num>
                          {formatBytes(
                            project.default_resource_limits.memory_bytes,
                          )}
                        </Num>
                      </td>
                    </tr>
                  </tbody>
                </table>
              ) : (
                <p className="text-faint">No default resource limits.</p>
              )}
            </div>

            <p className="text-faint">
              Shared env and default limits are set when the project is created.
            </p>
          </div>
        </section>

        {/* Members */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            members
          </p>
          {members.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Users size={22} />}
                title="No members yet"
                hint={
                  canManage
                    ? 'Add a user below to grant them access to this project.'
                    : 'This project has no members.'
                }
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>user</th>
                    <th>role</th>
                    {canManage ? <th aria-label="actions" /> : null}
                  </tr>
                </thead>
                <tbody>
                  {members.map((m) => (
                    <tr key={`${m.user_id}-${m.project_id}`}>
                      <td>
                        <span className="flex flex-col">
                          <span>{usernameById.get(m.user_id) ?? m.user_id}</span>
                          {usernameById.has(m.user_id) ? (
                            <Num
                              className="text-faint"
                              title={m.user_id}
                            >
                              <span style={{ fontSize: 'var(--text-label)' }}>
                                {m.user_id.slice(0, 8)}
                              </span>
                            </Num>
                          ) : null}
                        </span>
                      </td>
                      <td>
                        <span className="badge">{m.role}</span>
                      </td>
                      {canManage ? (
                        <td className="num">
                          <ConfirmButton
                            label="remove"
                            confirmLabel="Remove"
                            className="btn btn-icon"
                            message={`Remove ${usernameById.get(m.user_id) ?? m.user_id} from this project?`}
                            busy={removeMemberMutation.isPending}
                            onConfirm={() =>
                              removeMemberMutation.mutate(m.user_id)
                            }
                            align="right"
                          />
                        </td>
                      ) : null}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {canManage ? (
            <form
              className="panel panel-pad stack"
              style={{ marginTop: '1rem' }}
              onSubmit={(e) => {
                e.preventDefault()
                const userId = newUserId.trim()
                if (!userId) return
                addMemberMutation.mutate({ userId, role: newRole })
              }}
            >
              <p className="kicker m-0">add member</p>
              <div className="form-grid">
                <div className="col-span-12 sm:col-span-8 flex flex-col gap-1">
                  <label className="kicker" htmlFor="add-member-user">
                    user
                  </label>
                  <select
                    id="add-member-user"
                    value={newUserId}
                    onChange={(e) => setNewUserId(e.target.value)}
                    className="field-input w-full"
                    disabled={availableUsers.length === 0}
                  >
                    <option value="">
                      {availableUsers.length === 0
                        ? 'no users available'
                        : 'select a user'}
                    </option>
                    {availableUsers.map((u) => (
                      <option key={u.id} value={u.id}>
                        {u.username}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="col-span-12 sm:col-span-4 flex flex-col gap-1">
                  <label className="kicker" htmlFor="add-member-role">
                    role
                  </label>
                  <select
                    id="add-member-role"
                    value={newRole}
                    onChange={(e) => setNewRole(e.target.value as Role)}
                    className="field-input w-full"
                  >
                    <option value="viewer">viewer</option>
                    <option value="operator">operator</option>
                    <option value="admin">admin</option>
                  </select>
                </div>
              </div>
              <div className="cluster">
                <button
                  type="submit"
                  className="btn btn-primary"
                  disabled={addMemberMutation.isPending || !newUserId}
                >
                  {addMemberMutation.isPending ? (
                    <span className="spin" aria-hidden="true" />
                  ) : null}
                  {addMemberMutation.isPending ? 'adding' : 'add member'}
                </button>
              </div>
              {addMemberError ? <InlineError message={addMemberError} /> : null}
            </form>
          ) : null}
        </section>

        {/* Services */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            services <span className="text-faint tnum">{projectServices.length}</span>
          </p>
          {projectServices.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Boxes size={22} />}
                title="No services in this project"
                hint="Services created against this project appear here with their runtime status."
                action={
                  <Link to="/services" className="btn">
                    Go to services
                  </Link>
                }
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>service</th>
                    <th>status</th>
                    <th>routes</th>
                  </tr>
                </thead>
                <tbody>
                  {projectServices.map((svc) => {
                    const status = statusByService.get(svc.id)
                    return (
                      <tr key={svc.id}>
                        <td>
                          <Link
                            to="/services/$serviceId"
                            params={{ serviceId: svc.id }}
                            className="font-semibold no-underline hover:underline"
                          >
                            {svc.name}
                          </Link>
                        </td>
                        <td>
                          {status ? (
                            <StatusBadge status={status} />
                          ) : (
                            <span className="text-faint">—</span>
                          )}
                        </td>
                        <td className="text-faint" style={{ fontSize: 'var(--text-label)' }}>
                          {svc.domains.length > 0
                            ? svc.domains.join(', ')
                            : `:${svc.internal_port}`}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>

        {/* Registries */}
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            registries
          </p>
          <div className="panel panel-pad stack">
            <p className="text-faint" style={{ maxWidth: '60ch' }}>
              Container registries are managed on the Registries page for the
              selected project.
            </p>
            <div className="cluster">
              <button
                type="button"
                className="btn"
                onClick={() => {
                  setActiveProject(projectId)
                  navigate({ to: '/registries' })
                }}
              >
                Open registries
              </button>
            </div>
          </div>
        </section>
      </div>
    </main>
  )
}
