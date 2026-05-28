import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import type { RegistryInput, Role } from '#/effect/schema'

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

const listRegistries = (projectId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listRegistries(projectId)
  })

const createRegistry = (projectId: string, input: RegistryInput) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createRegistry(projectId, input)
  })

const updateRegistry = (
  projectId: string,
  registryId: string,
  input: RegistryInput,
) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.updateRegistry(projectId, registryId, input)
  })

const deleteRegistry = (projectId: string, registryId: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteRegistry(projectId, registryId)
  })

const AUTH_KINDS: ReadonlyArray<[RegistryInput['auth_kind'], string]> = [
  ['anonymous', 'Anonymous'],
  ['basic', 'Basic'],
  ['token', 'Token'],
  ['ecr_token', 'ECR Token'],
  ['gar_token', 'GAR Token'],
]

export const Route = createFileRoute('/projects/$projectId')({
  component: ProjectDetail,
})

export function ProjectDetail() {
  const { projectId } = Route.useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { isSuperAdmin, roleForActiveProject } = useAuth()
  const [deleteError, setDeleteError] = useState('')
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [newUserId, setNewUserId] = useState<string>('')
  const [newRole, setNewRole] = useState<Role>('viewer')

  const { data: project, isFetching } = useQuery({
    queryKey: ['projects', projectId],
    queryFn: () => runQuery(getProject(projectId)),
  })

  const canManage =
    isSuperAdmin || roleForActiveProject(projectId) === 'admin'

  const { data: members = [] } = useQuery({
    queryKey: ['projects', projectId, 'members'],
    queryFn: () => runQuery(listMembers(projectId)),
    enabled: projectId.length > 0,
  })

  const addMemberMutation = useMutation({
    mutationFn: (input: { userId: string; role: Role }) =>
      runQuery(addMember(projectId, input.userId, input.role)),
    onSuccess: () => {
      setNewUserId('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
  })

  const removeMemberMutation = useMutation({
    mutationFn: (userId: string) =>
      runQuery(removeMember(projectId, userId)),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
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

  const [regName, setRegName] = useState('')
  const [regEndpoint, setRegEndpoint] = useState('')
  const [regAuthKind, setRegAuthKind] =
    useState<RegistryInput['auth_kind']>('anonymous')
  const [regCredRef, setRegCredRef] = useState('')
  const [regCreateError, setRegCreateError] = useState('')
  const [regDeleteConfirm, setRegDeleteConfirm] = useState<string | null>(null)
  const [regDeleteError, setRegDeleteError] = useState('')

  const [regEditId, setRegEditId] = useState<string | null>(null)
  const [editName, setEditName] = useState('')
  const [editEndpoint, setEditEndpoint] = useState('')
  const [editAuthKind, setEditAuthKind] =
    useState<RegistryInput['auth_kind']>('anonymous')
  const [editCredRef, setEditCredRef] = useState('')
  const [regEditError, setRegEditError] = useState('')

  const { data: registries = [] } = useQuery({
    queryKey: ['projects', projectId, 'registries'],
    queryFn: () => runQuery(listRegistries(projectId)),
    enabled: projectId.length > 0 && canManage,
  })

  const createRegMutation = useMutation({
    mutationFn: (input: RegistryInput) =>
      runQuery(createRegistry(projectId, input)),
    onSuccess: () => {
      setRegName('')
      setRegEndpoint('')
      setRegAuthKind('anonymous')
      setRegCredRef('')
      setRegCreateError('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (error: unknown) => {
      const msg =
        error instanceof Error ? error.message : 'Failed to create registry'
      setRegCreateError(msg)
    },
  })

  const updateRegMutation = useMutation({
    mutationFn: (input: { registryId: string; body: RegistryInput }) =>
      runQuery(updateRegistry(projectId, input.registryId, input.body)),
    onSuccess: () => {
      setRegEditId(null)
      setRegEditError('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (error: unknown) => {
      const msg =
        error instanceof Error ? error.message : 'Failed to update registry'
      setRegEditError(msg)
    },
  })

  const startEditRegistry = (r: {
    id: string
    name: string
    endpoint: string
    auth_kind: RegistryInput['auth_kind']
    credential_ref: string | null
  }) => {
    setRegEditId(r.id)
    setRegEditError('')
    setEditName(r.name)
    setEditEndpoint(r.endpoint)
    setEditAuthKind(r.auth_kind)
    setEditCredRef(r.credential_ref ?? '')
  }

  const deleteRegMutation = useMutation({
    mutationFn: (registryId: string) =>
      runQuery(deleteRegistry(projectId, registryId)),
    onSuccess: () => {
      setRegDeleteConfirm(null)
      setRegDeleteError('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (error: unknown) => {
      const msg =
        error instanceof Error ? error.message : ''
      setRegDeleteError(
        msg.toLowerCase().includes('in use')
          ? 'Registry is in use by one or more services.'
          : msg,
      )
      setRegDeleteConfirm(null)
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
        <h2 className="kicker border-b border-[var(--border)] px-4 py-2.5">
          shared environment
        </h2>
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
          <h2 className="kicker border-b border-[var(--border)] px-4 py-2.5">
            default resource limits
          </h2>
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

      <section className="panel mb-8 overflow-hidden">
        <h2 className="kicker border-b border-[var(--border)] px-4 py-2.5">
          members
        </h2>
        {members.length === 0 ? (
          <p className="px-4 py-3 text-sm text-[var(--fg-muted)]">
            No members yet.
          </p>
        ) : (
          <ul className="m-0 list-none">
            {members.map((m, i) => (
              <li
                key={`${m.user_id}-${m.project_id}`}
                className={`flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-2.5 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span className="font-mono text-xs text-[var(--fg-muted)]">
                  {m.user_id}
                </span>
                <span className="kicker">{m.role}</span>
                {canManage ? (
                  <button
                    type="button"
                    className="btn text-xs ml-auto"
                    onClick={() => removeMemberMutation.mutate(m.user_id)}
                    disabled={removeMemberMutation.isPending}
                  >
                    remove
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
        {canManage ? (
          <form
            className="flex flex-wrap items-end gap-2 border-t border-[var(--border)] px-4 py-3"
            onSubmit={(e) => {
              e.preventDefault()
              const userId = newUserId.trim()
              if (!userId) return
              addMemberMutation.mutate({ userId, role: newRole })
            }}
          >
            <label htmlFor="add-member-user" className="sr-only">
              User id (uuid)
            </label>
            <input
              id="add-member-user"
              type="text"
              placeholder="user id (uuid)"
              value={newUserId}
              onChange={(e) => setNewUserId(e.target.value)}
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono"
            />
            <label htmlFor="add-member-role" className="sr-only">
              Member role
            </label>
            <select
              id="add-member-role"
              value={newRole}
              onChange={(e) => setNewRole(e.target.value as Role)}
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm"
            >
              <option value="viewer">viewer</option>
              <option value="operator">operator</option>
              <option value="admin">admin</option>
            </select>
            <button
              type="submit"
              className="btn btn-primary text-xs"
              disabled={addMemberMutation.isPending}
            >
              {addMemberMutation.isPending ? 'adding…' : 'add member'}
            </button>
          </form>
        ) : null}
      </section>

      {canManage ? (
        <section className="panel mb-8 overflow-hidden">
          <h2 className="kicker border-b border-[var(--border)] px-4 py-2.5">
            registries
          </h2>

          {regDeleteError ? (
            <div role="alert" className="px-4 py-2 text-xs text-[var(--violet)]">
              <span className="signal signal-fault mr-2 inline-block align-middle" />
              {regDeleteError}
            </div>
          ) : null}

          {registries.length === 0 ? (
            <p className="px-4 py-3 text-sm text-[var(--fg-muted)]">
              No registries configured.
            </p>
          ) : (
            <ul className="m-0 list-none">
              {registries.map((r, i) => (
                <li
                  key={r.id}
                  className={`px-4 py-2.5 text-sm ${
                    i > 0 ? 'border-t border-[var(--border)]' : ''
                  }`}
                >
                  {regEditId === r.id ? (
                    <form
                      className="flex flex-wrap items-end gap-2"
                      onSubmit={(e) => {
                        e.preventDefault()
                        const name = editName.trim()
                        const endpoint = editEndpoint.trim()
                        if (!name || !endpoint) return
                        updateRegMutation.mutate({
                          registryId: r.id,
                          body: {
                            name,
                            endpoint,
                            auth_kind: editAuthKind,
                            secret_ref:
                              editCredRef.trim().length > 0
                                ? editCredRef.trim()
                                : null,
                          },
                        })
                      }}
                    >
                      <input
                        type="text"
                        aria-label="edit registry name"
                        placeholder="name"
                        value={editName}
                        onChange={(e) => setEditName(e.target.value)}
                        className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
                      />
                      <input
                        type="text"
                        aria-label="edit registry endpoint"
                        placeholder="endpoint"
                        value={editEndpoint}
                        onChange={(e) => setEditEndpoint(e.target.value)}
                        className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
                      />
                      <select
                        aria-label="edit registry auth kind"
                        value={editAuthKind}
                        onChange={(e) =>
                          setEditAuthKind(
                            e.target.value as RegistryInput['auth_kind'],
                          )
                        }
                        className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm text-[var(--fg)]"
                      >
                        {AUTH_KINDS.map(([value, label]) => (
                          <option key={value} value={value}>
                            {label}
                          </option>
                        ))}
                      </select>
                      <input
                        type="text"
                        aria-label="edit registry credential ref"
                        placeholder="credential ref (optional)"
                        value={editCredRef}
                        onChange={(e) => setEditCredRef(e.target.value)}
                        className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
                      />
                      <button
                        type="submit"
                        className="btn btn-primary text-xs"
                        disabled={
                          updateRegMutation.isPending ||
                          editName.trim().length === 0 ||
                          editEndpoint.trim().length === 0
                        }
                      >
                        {updateRegMutation.isPending ? 'saving...' : 'save'}
                      </button>
                      <button
                        type="button"
                        className="btn text-xs"
                        onClick={() => {
                          setRegEditId(null)
                          setRegEditError('')
                        }}
                      >
                        cancel
                      </button>
                      {regEditError ? (
                        <span className="w-full text-xs text-[var(--violet)]">
                          {regEditError}
                        </span>
                      ) : null}
                    </form>
                  ) : (
                    <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
                      <span className="font-semibold text-[var(--fg)]">
                        {r.name}
                      </span>
                      <span className="tnum text-xs text-[var(--fg-muted)]">
                        {r.endpoint}
                      </span>
                      <span className="kicker">{r.auth_kind}</span>
                      {r.credential_ref ? (
                        <span className="text-xs text-[var(--fg-muted)]">
                          cred: {r.credential_ref}
                        </span>
                      ) : null}

                      {regDeleteConfirm === r.id ? (
                        <span className="inline-flex items-center gap-1 text-xs ml-auto">
                          <span className="text-[var(--violet)]">remove?</span>
                          <button
                            type="button"
                            className="btn text-xs"
                            onClick={() => {
                              deleteRegMutation.mutate(r.id)
                            }}
                            disabled={deleteRegMutation.isPending}
                          >
                            yes
                          </button>
                          <button
                            type="button"
                            className="btn text-xs"
                            onClick={() => setRegDeleteConfirm(null)}
                          >
                            no
                          </button>
                        </span>
                      ) : (
                        <span className="ml-auto inline-flex items-center gap-1">
                          <button
                            type="button"
                            className="btn text-xs"
                            onClick={() => startEditRegistry(r)}
                          >
                            edit
                          </button>
                          <button
                            type="button"
                            className="btn text-xs"
                            onClick={() => setRegDeleteConfirm(r.id)}
                          >
                            delete
                          </button>
                        </span>
                      )}
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}

          {regCreateError ? (
            <div role="alert" className="border-t border-[var(--border)] px-4 py-2 text-xs text-[var(--violet)]">
              {regCreateError}
            </div>
          ) : null}

          <form
            className="flex flex-wrap items-end gap-2 border-t border-[var(--border)] px-4 py-3"
            onSubmit={(e) => {
              e.preventDefault()
              const name = regName.trim()
              const endpoint = regEndpoint.trim()
              if (!name || !endpoint) return
              createRegMutation.mutate({
                name,
                endpoint,
                auth_kind: regAuthKind,
                secret_ref:
                  regCredRef.trim().length > 0
                    ? regCredRef.trim()
                    : null,
              })
            }}
          >
            <label htmlFor="reg-name" className="sr-only">
              Registry name
            </label>
            <input
              id="reg-name"
              type="text"
              placeholder="name"
              value={regName}
              onChange={(e) => setRegName(e.target.value)}
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
            />
            <label htmlFor="reg-endpoint" className="sr-only">
              Registry endpoint
            </label>
            <input
              id="reg-endpoint"
              type="text"
              placeholder="endpoint"
              value={regEndpoint}
              onChange={(e) => setRegEndpoint(e.target.value)}
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
            />
            <label htmlFor="reg-auth-kind" className="sr-only">
              Auth kind
            </label>
            <select
              id="reg-auth-kind"
              value={regAuthKind}
              onChange={(e) =>
                setRegAuthKind(e.target.value as RegistryInput['auth_kind'])
              }
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm text-[var(--fg)]"
            >
              {AUTH_KINDS.map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
            <label htmlFor="reg-cred-ref" className="sr-only">
              Credential ref (optional)
            </label>
            <input
              id="reg-cred-ref"
              type="text"
              placeholder="credential ref (optional)"
              value={regCredRef}
              onChange={(e) => setRegCredRef(e.target.value)}
              className="border border-[var(--border)] bg-transparent px-2 py-2 text-sm font-mono text-[var(--fg)]"
            />
            <button
              type="submit"
              className="btn btn-primary text-xs"
              disabled={
                createRegMutation.isPending ||
                regName.trim().length === 0 ||
                regEndpoint.trim().length === 0
              }
            >
              {createRegMutation.isPending ? 'creating...' : 'add registry'}
            </button>
          </form>
        </section>
      ) : null}

      <section className="mb-8">
        {deleteError && (
          <div role="alert" className="panel mb-4 p-4">
            <p className="m-0 flex items-center gap-2 text-sm signal-fault">
              <span className="signal signal-fault" />
              {deleteError}
            </p>
          </div>
        )}
        {confirmDelete ? (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm text-[var(--violet)]">
              Delete project &quot;{project.name}&quot;? This cannot be undone.
            </span>
            <button
              type="button"
              className="btn text-xs"
              onClick={() => {
                setDeleteError('')
                setConfirmDelete(false)
                del.mutate()
              }}
              disabled={del.isPending}
            >
              {del.isPending ? 'deleting...' : 'confirm delete'}
            </button>
            <button
              type="button"
              className="btn text-xs"
              onClick={() => setConfirmDelete(false)}
              disabled={del.isPending}
            >
              cancel
            </button>
          </div>
        ) : (
          <button
            type="button"
            className="btn"
            onClick={() => setConfirmDelete(true)}
            disabled={del.isPending}
          >
            delete project
          </button>
        )}
      </section>
    </main>
  )
}
