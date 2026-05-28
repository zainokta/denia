import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { FieldHint } from '#/components/FieldHint'
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

const listUserDirectory = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listUserDirectory
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

const authKindLabel = (k: RegistryInput['auth_kind']) =>
  AUTH_KINDS.find(([value]) => value === k)?.[1] ?? k

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
  const [addMemberError, setAddMemberError] = useState('')
  const [removeMemberError, setRemoveMemberError] = useState('')
  const [memberRemoveConfirm, setMemberRemoveConfirm] = useState<string | null>(null)

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

  const { data: userDirectory = [] } = useQuery({
    queryKey: ['users', 'directory'],
    queryFn: () => runQuery(listUserDirectory),
    enabled: canManage,
  })

  const usernameById = new Map(userDirectory.map((u) => [u.id, u.username]))
  const memberIds = new Set(members.map((m) => m.user_id))
  const availableUsers = userDirectory.filter((u) => !memberIds.has(u.id))


  const addMemberMutation = useMutation({
    mutationFn: (input: { userId: string; role: Role }) =>
      runQuery(addMember(projectId, input.userId, input.role)),
    onSuccess: () => {
      setNewUserId('')
      setAddMemberError('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
    onError: (error: unknown) => {
      setAddMemberError(
        error instanceof Error ? error.message : 'Failed to add member',
      )
    },
  })

  const removeMemberMutation = useMutation({
    mutationFn: (userId: string) =>
      runQuery(removeMember(projectId, userId)),
    onSuccess: () => {
      setMemberRemoveConfirm(null)
      setRemoveMemberError('')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'members'],
      })
    },
    onError: (error: unknown) => {
      setRemoveMemberError(
        error instanceof Error ? error.message : 'Failed to remove member',
      )
      setMemberRemoveConfirm(null)
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
  const [regUsername, setRegUsername] = useState('')
  const [regPassword, setRegPassword] = useState('')
  const [regToken, setRegToken] = useState('')
  const [regCreateError, setRegCreateError] = useState('')
  const [regDeleteConfirm, setRegDeleteConfirm] = useState<string | null>(null)
  const [regDeleteError, setRegDeleteError] = useState('')

  const [regEditId, setRegEditId] = useState<string | null>(null)
  const [editName, setEditName] = useState('')
  const [editEndpoint, setEditEndpoint] = useState('')
  const [editAuthKind, setEditAuthKind] =
    useState<RegistryInput['auth_kind']>('anonymous')
  const [editUsername, setEditUsername] = useState('')
  const [editPassword, setEditPassword] = useState('')
  const [editToken, setEditToken] = useState('')
  const [regEditError, setRegEditError] = useState('')

  const buildRegistryPayload = (
    name: string,
    endpoint: string,
    authKind: RegistryInput['auth_kind'],
    username: string,
    password: string,
    token: string,
  ): RegistryInput | { error: string } => {
    const base = { name, endpoint, auth_kind: authKind } as RegistryInput
    switch (authKind) {
      case 'anonymous':
        return base
      case 'basic':
        if (username.trim().length === 0 || password.length === 0) {
          return { error: 'Username and password are required for basic auth.' }
        }
        return { ...base, username: username.trim(), password }
      case 'token':
      case 'ecr_token':
      case 'gar_token':
        if (token.length === 0) {
          return { error: 'Token is required for this auth kind.' }
        }
        return { ...base, token }
    }
  }

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
      setRegUsername('')
      setRegPassword('')
      setRegToken('')
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
    // Always cleared: editing never reveals the existing credential. PATCH
    // overwrites the stored secret only if new fields are filled in.
    setEditUsername('')
    setEditPassword('')
    setEditToken('')
  }

  const [regDeleteErrorFor, setRegDeleteErrorFor] = useState<string | null>(null)

  const deleteRegMutation = useMutation({
    mutationFn: (registryId: string) =>
      runQuery(deleteRegistry(projectId, registryId)),
    onSuccess: () => {
      setRegDeleteConfirm(null)
      setRegDeleteError('')
      setRegDeleteErrorFor(null)
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (error: unknown, registryId: string) => {
      const msg =
        error instanceof Error ? error.message : ''
      setRegDeleteError(
        msg.toLowerCase().includes('in use')
          ? 'Registry is in use by one or more services.'
          : msg || 'Failed to delete registry.',
      )
      setRegDeleteErrorFor(registryId)
      setRegDeleteConfirm(null)
    },
  })

  if (isFetching && !project) {
    return (
      <main className="page-wrap px-4 pb-12 pt-12" aria-busy="true">
        <p className="kicker mb-3">projects</p>
        <p className="text-[var(--fg-muted)]" aria-live="polite">
          loading...
        </p>
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
      <nav aria-label="Breadcrumb" className="mb-3">
        <ol className="kicker m-0 flex list-none flex-wrap items-center gap-x-2 p-0">
          <li>
            <Link to="/projects" className="no-underline hover:underline">
              projects
            </Link>
          </li>
          <li aria-hidden="true">/</li>
          <li aria-current="page">{project.name}</li>
        </ol>
      </nav>
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

        {removeMemberError ? (
          <div
            role="alert"
            className="border-b border-[var(--border)] px-4 py-2 text-xs text-[var(--violet)]"
          >
            <span className="signal signal-fault mr-2 inline-block align-middle" />
            {removeMemberError}
          </div>
        ) : null}

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
                <span className="flex flex-col">
                  <span className="text-sm text-[var(--fg)]">
                    {usernameById.get(m.user_id) ?? m.user_id}
                  </span>
                  {usernameById.has(m.user_id) ? (
                    <span
                      className="font-mono text-[10px] text-[var(--fg-muted)]"
                      title={m.user_id}
                    >
                      {m.user_id.slice(0, 8)}…
                    </span>
                  ) : null}
                </span>
                <span className="kicker">{m.role}</span>
                {canManage ? (
                  memberRemoveConfirm === m.user_id ? (
                    <span className="inline-flex items-center gap-1 text-xs ml-auto">
                      <span className="text-[var(--violet)]">remove?</span>
                      <button
                        type="button"
                        className="btn btn-danger text-xs"
                        onClick={() => removeMemberMutation.mutate(m.user_id)}
                        disabled={removeMemberMutation.isPending}
                      >
                        yes
                      </button>
                      <button
                        type="button"
                        className="btn text-xs"
                        onClick={() => setMemberRemoveConfirm(null)}
                      >
                        no
                      </button>
                    </span>
                  ) : (
                    <button
                      type="button"
                      className="btn text-xs ml-auto"
                      onClick={() => {
                        setRemoveMemberError('')
                        setMemberRemoveConfirm(m.user_id)
                      }}
                      disabled={removeMemberMutation.isPending}
                    >
                      remove
                    </button>
                  )
                ) : null}
              </li>
            ))}
          </ul>
        )}
        {canManage ? (
          <>
            {addMemberError ? (
              <div
                role="alert"
                className="border-t border-[var(--border)] px-4 py-2 text-xs text-[var(--violet)]"
              >
                {addMemberError}
              </div>
            ) : null}
            <form
              className="border-t border-[var(--border)] px-4 py-3"
              onSubmit={(e) => {
                e.preventDefault()
                const userId = newUserId.trim()
                if (!userId) return
                addMemberMutation.mutate({ userId, role: newRole })
              }}
            >
              <div className="form-grid mb-3">
                <div className="flex flex-col gap-1 col-span-12 sm:col-span-8">
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
                        : 'select a user…'}
                    </option>
                    {availableUsers.map((u) => (
                      <option key={u.id} value={u.id}>
                        {u.username}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
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
              <button
                type="submit"
                className="btn btn-primary text-xs"
                disabled={addMemberMutation.isPending || !newUserId}
              >
                {addMemberMutation.isPending ? 'adding...' : 'add member'}
              </button>
            </form>
          </>
        ) : null}
      </section>

      {canManage ? (
        <section className="panel mb-8 overflow-hidden">
          <h2 className="kicker border-b border-[var(--border)] px-4 py-2.5">
            registries
          </h2>

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
                      onSubmit={(e) => {
                        e.preventDefault()
                        const name = editName.trim()
                        const endpoint = editEndpoint.trim()
                        if (!name || !endpoint) return
                        const built = buildRegistryPayload(
                          name,
                          endpoint,
                          editAuthKind,
                          editUsername,
                          editPassword,
                          editToken,
                        )
                        if ('error' in built) {
                          setRegEditError(built.error)
                          return
                        }
                        setRegEditError('')
                        updateRegMutation.mutate({
                          registryId: r.id,
                          body: built,
                        })
                      }}
                    >
                      <div className="form-grid mb-3">
                        <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
                          <label className="kicker" htmlFor={`reg-edit-name-${r.id}`}>
                            name
                          </label>
                          <input
                            id={`reg-edit-name-${r.id}`}
                            type="text"
                            aria-label="edit registry name"
                            placeholder="ghcr-prod"
                            value={editName}
                            onChange={(e) => setEditName(e.target.value)}
                            className="field-input w-full"
                          />
                        </div>
                        <div className="flex flex-col gap-1 col-span-12 sm:col-span-8">
                          <div className="flex items-center gap-1.5">
                            <label className="kicker" htmlFor={`reg-edit-endpoint-${r.id}`}>
                              endpoint
                            </label>
                            <FieldHint
                              id={`hint-reg-edit-endpoint-${r.id}`}
                              label="about registry endpoint"
                            >
                              Registry hostname, e.g. <code>ghcr.io</code> or{' '}
                              <code>registry.gitea.example</code>. Do not include
                              the image path.
                            </FieldHint>
                          </div>
                          <input
                            id={`reg-edit-endpoint-${r.id}`}
                            type="text"
                            aria-label="edit registry endpoint"
                            placeholder="ghcr.io"
                            value={editEndpoint}
                            onChange={(e) => setEditEndpoint(e.target.value)}
                            className="field-input w-full"
                          />
                        </div>
                        <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                          <div className="flex items-center gap-1.5">
                            <label className="kicker" htmlFor={`reg-edit-auth-${r.id}`}>
                              auth kind
                            </label>
                            <FieldHint
                              id={`hint-reg-edit-auth-${r.id}`}
                              label="about auth kind"
                            >
                              <code>anonymous</code> public pulls;{' '}
                              <code>basic</code> HTTP Basic (user/pass);{' '}
                              <code>token</code> static bearer; <code>ecr_token</code>{' '}
                              AWS ECR get-login flow; <code>gar_token</code> Google
                              Artifact Registry.
                            </FieldHint>
                          </div>
                          <select
                            id={`reg-edit-auth-${r.id}`}
                            aria-label="edit registry auth kind"
                            value={editAuthKind}
                            onChange={(e) =>
                              setEditAuthKind(
                                e.target.value as RegistryInput['auth_kind'],
                              )
                            }
                            className="field-input w-full"
                          >
                            {AUTH_KINDS.map(([value, label]) => (
                              <option key={value} value={value}>
                                {label}
                              </option>
                            ))}
                          </select>
                        </div>
                        {editAuthKind === 'basic' ? (
                          <>
                            <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                              <label
                                className="kicker"
                                htmlFor={`reg-edit-username-${r.id}`}
                              >
                                username
                              </label>
                              <input
                                id={`reg-edit-username-${r.id}`}
                                type="text"
                                aria-label="edit registry username"
                                autoComplete="off"
                                placeholder="ci-bot"
                                value={editUsername}
                                onChange={(e) =>
                                  setEditUsername(e.target.value)
                                }
                                className="field-input w-full"
                              />
                            </div>
                            <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                              <label
                                className="kicker"
                                htmlFor={`reg-edit-password-${r.id}`}
                              >
                                password
                              </label>
                              <input
                                id={`reg-edit-password-${r.id}`}
                                type="password"
                                aria-label="edit registry password"
                                autoComplete="new-password"
                                placeholder="••••••••"
                                value={editPassword}
                                onChange={(e) =>
                                  setEditPassword(e.target.value)
                                }
                                className="field-input w-full"
                              />
                            </div>
                          </>
                        ) : editAuthKind === 'anonymous' ? null : (
                          <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                            <label
                              className="kicker"
                              htmlFor={`reg-edit-token-${r.id}`}
                            >
                              token
                            </label>
                            <input
                              id={`reg-edit-token-${r.id}`}
                              type="password"
                              aria-label="edit registry token"
                              autoComplete="new-password"
                              placeholder="••••••••"
                              value={editToken}
                              onChange={(e) => setEditToken(e.target.value)}
                              className="field-input w-full"
                            />
                          </div>
                        )}
                      </div>
                      <div className="flex flex-wrap items-center gap-2">
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
                          <span className="text-xs text-[var(--violet)]">
                            {regEditError}
                          </span>
                        ) : null}
                      </div>
                    </form>
                  ) : (
                    <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
                      <span className="font-semibold text-[var(--fg)]">
                        {r.name}
                      </span>
                      <span className="tnum text-xs text-[var(--fg-muted)]">
                        {r.endpoint}
                      </span>
                      <span className="kicker">{authKindLabel(r.auth_kind)}</span>
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
                            className="btn btn-danger text-xs"
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
                            onClick={() => {
                              setRegDeleteError('')
                              setRegDeleteErrorFor(null)
                              setRegDeleteConfirm(r.id)
                            }}
                          >
                            delete
                          </button>
                        </span>
                      )}
                    </div>
                  )}
                  {regDeleteErrorFor === r.id && regDeleteError ? (
                    <div
                      role="alert"
                      className="mt-2 text-xs text-[var(--violet)]"
                    >
                      <span className="signal signal-fault mr-2 inline-block align-middle" />
                      {regDeleteError}
                    </div>
                  ) : null}
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
            className="border-t border-[var(--border)] px-4 py-3"
            onSubmit={(e) => {
              e.preventDefault()
              const name = regName.trim()
              const endpoint = regEndpoint.trim()
              if (!name || !endpoint) return
              const built = buildRegistryPayload(
                name,
                endpoint,
                regAuthKind,
                regUsername,
                regPassword,
                regToken,
              )
              if ('error' in built) {
                setRegCreateError(built.error)
                return
              }
              setRegCreateError('')
              createRegMutation.mutate(built)
            }}
          >
            <div className="form-grid mb-3">
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
                <label className="kicker" htmlFor="reg-name">
                  name
                </label>
                <input
                  id="reg-name"
                  type="text"
                  placeholder="ghcr-prod"
                  value={regName}
                  onChange={(e) => setRegName(e.target.value)}
                  className="field-input w-full"
                />
              </div>
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-8">
                <div className="flex items-center gap-1.5">
                  <label className="kicker" htmlFor="reg-endpoint">
                    endpoint
                  </label>
                  <FieldHint
                    id="hint-reg-endpoint"
                    label="about registry endpoint"
                  >
                    Registry hostname, e.g. <code>ghcr.io</code> or{' '}
                    <code>registry.gitea.example</code>. Do not include the
                    image path.
                  </FieldHint>
                </div>
                <input
                  id="reg-endpoint"
                  type="text"
                  placeholder="ghcr.io"
                  value={regEndpoint}
                  onChange={(e) => setRegEndpoint(e.target.value)}
                  className="field-input w-full"
                />
              </div>
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                <div className="flex items-center gap-1.5">
                  <label className="kicker" htmlFor="reg-auth-kind">
                    auth kind
                  </label>
                  <FieldHint
                    id="hint-reg-auth-kind"
                    label="about auth kind"
                  >
                    <code>anonymous</code> public pulls; <code>basic</code> HTTP
                    Basic (user/pass); <code>token</code> static bearer;{' '}
                    <code>ecr_token</code> AWS ECR get-login flow;{' '}
                    <code>gar_token</code> Google Artifact Registry.
                  </FieldHint>
                </div>
                <select
                  id="reg-auth-kind"
                  value={regAuthKind}
                  onChange={(e) =>
                    setRegAuthKind(e.target.value as RegistryInput['auth_kind'])
                  }
                  className="field-input w-full"
                >
                  {AUTH_KINDS.map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </select>
              </div>
              {regAuthKind === 'basic' ? (
                <>
                  <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                    <label className="kicker" htmlFor="reg-username">
                      username
                    </label>
                    <input
                      id="reg-username"
                      type="text"
                      autoComplete="off"
                      placeholder="ci-bot"
                      value={regUsername}
                      onChange={(e) => setRegUsername(e.target.value)}
                      className="field-input w-full"
                    />
                  </div>
                  <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                    <label className="kicker" htmlFor="reg-password">
                      password
                    </label>
                    <input
                      id="reg-password"
                      type="password"
                      autoComplete="new-password"
                      placeholder="••••••••"
                      value={regPassword}
                      onChange={(e) => setRegPassword(e.target.value)}
                      className="field-input w-full"
                    />
                  </div>
                </>
              ) : regAuthKind === 'anonymous' ? null : (
                <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                  <div className="flex items-center gap-1.5">
                    <label className="kicker" htmlFor="reg-token">
                      token
                    </label>
                    <FieldHint id="hint-reg-token" label="about token">
                      Encrypted server-side via SOPS (ADR-021). Stored
                      <code>0600</code> under the project secrets directory.
                    </FieldHint>
                  </div>
                  <input
                    id="reg-token"
                    type="password"
                    autoComplete="new-password"
                    placeholder="••••••••"
                    value={regToken}
                    onChange={(e) => setRegToken(e.target.value)}
                    className="field-input w-full"
                  />
                </div>
              )}
            </div>
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
              className="btn btn-danger text-xs"
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
