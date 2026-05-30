import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { Container } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useActiveProject } from '#/hooks/useActiveProject'
import { useAuth } from '#/hooks/useAuth'
import { FieldHint } from '#/components/FieldHint'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { ErrorPanel, InlineError, errorMessage } from '#/components/ErrorPanel'
import { ConfirmButton } from '#/components/ConfirmButton'
import { CopyButton } from '#/components/CopyButton'
import { useActionToasts } from '#/components/Toast'
import { Num } from '#/components/Num'
import type { RegistryInput } from '#/effect/schema'

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
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

// Inline-payload shape gated on auth_kind. On edit, leaving the credential
// fields blank keeps the existing secret (PATCH only overwrites when filled).
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

export const Route = createFileRoute('/registries')({ component: Registries })

function PageHeader({ projectName }: { projectName?: string }) {
  return (
    <header className="panel-head" style={{ marginBottom: '1.5rem' }}>
      <div>
        <p className="kicker">registries</p>
        <h1 className="t-display">Registries</h1>
      </div>
      {projectName ? <span className="badge">{projectName}</span> : null}
    </header>
  )
}

export function Registries() {
  const [projectId] = useActiveProject()
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const { isSuperAdmin, roleForActiveProject } = useAuth()

  const { data: projects = [] } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })
  const project = projects.find((p) => p.id === projectId)

  const canManage =
    projectId.length > 0 &&
    (isSuperAdmin || roleForActiveProject(projectId) === 'admin')

  // Add-registry form state.
  const [regName, setRegName] = useState('')
  const [regEndpoint, setRegEndpoint] = useState('')
  const [regAuthKind, setRegAuthKind] =
    useState<RegistryInput['auth_kind']>('anonymous')
  const [regUsername, setRegUsername] = useState('')
  const [regPassword, setRegPassword] = useState('')
  const [regToken, setRegToken] = useState('')
  const [regCreateError, setRegCreateError] = useState('')

  // Inline-edit form state.
  const [regEditId, setRegEditId] = useState<string | null>(null)
  const [editName, setEditName] = useState('')
  const [editEndpoint, setEditEndpoint] = useState('')
  const [editAuthKind, setEditAuthKind] =
    useState<RegistryInput['auth_kind']>('anonymous')
  const [editUsername, setEditUsername] = useState('')
  const [editPassword, setEditPassword] = useState('')
  const [editToken, setEditToken] = useState('')
  const [regEditError, setRegEditError] = useState('')

  // Row-level delete error.
  const [regDeleteErrorFor, setRegDeleteErrorFor] = useState<string | null>(
    null,
  )
  const [regDeleteError, setRegDeleteError] = useState('')

  const {
    data: registries = [],
    isLoading,
    isError,
    error,
    refetch,
  } = useQuery({
    queryKey: ['projects', projectId, 'registries'],
    queryFn: () => runQuery(listRegistries(projectId)),
    enabled: canManage,
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
      toast.ok('Registry added')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (err: unknown) => {
      const msg = errorMessage(err) || 'Failed to create registry'
      setRegCreateError(msg)
      toast.err(msg)
    },
  })

  const updateRegMutation = useMutation({
    mutationFn: (input: { registryId: string; body: RegistryInput }) =>
      runQuery(updateRegistry(projectId, input.registryId, input.body)),
    onSuccess: () => {
      setRegEditId(null)
      setRegEditError('')
      toast.ok('Registry updated')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (err: unknown) => {
      const msg = errorMessage(err) || 'Failed to update registry'
      setRegEditError(msg)
      toast.err(msg)
    },
  })

  const deleteRegMutation = useMutation({
    mutationFn: (registryId: string) =>
      runQuery(deleteRegistry(projectId, registryId)),
    onSuccess: () => {
      setRegDeleteError('')
      setRegDeleteErrorFor(null)
      toast.ok('Registry removed')
      queryClient.invalidateQueries({
        queryKey: ['projects', projectId, 'registries'],
      })
    },
    onError: (err: unknown, registryId: string) => {
      const msg = errorMessage(err)
      const display = msg.toLowerCase().includes('in use')
        ? 'Registry is in use by one or more services.'
        : msg || 'Failed to delete registry.'
      setRegDeleteError(display)
      setRegDeleteErrorFor(registryId)
      toast.err(display)
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

  // No project selected: explain how to pick one rather than crashing.
  if (projectId.length === 0) {
    return (
      <main className="page-wrap px-4 pb-16 pt-10">
        <PageHeader />
        <div className="panel">
          <EmptyState
            icon={<Container size={22} />}
            title="No project selected"
            hint="Choose a project from the switcher in the top bar to manage its registries."
          />
        </div>
      </main>
    )
  }

  // Selected but not an admin: registries are credential-bearing, admin-only.
  if (!canManage) {
    return (
      <main className="page-wrap px-4 pb-16 pt-10">
        <PageHeader projectName={project?.name} />
        <div className="panel">
          <EmptyState
            icon={<Container size={22} />}
            title="Admin role required"
            hint="Registry credentials are managed by project admins."
          />
        </div>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <PageHeader projectName={project?.name} />

      <div className="stack-lg">
        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            configured registries
          </p>

          {isLoading ? (
            <SkeletonRows rows={3} />
          ) : isError ? (
            <ErrorPanel
              title="Could not load registries"
              message={errorMessage(error)}
              onRetry={() => refetch()}
            />
          ) : registries.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Container size={22} />}
                title="No registries"
                hint="Add one so this project's services can pull private images."
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>name</th>
                    <th>endpoint</th>
                    <th>auth kind</th>
                    <th>credential ref</th>
                    <th aria-label="actions" />
                  </tr>
                </thead>
                <tbody>
                  {registries.map((r) =>
                    regEditId === r.id ? (
                      <tr key={r.id}>
                        <td colSpan={5}>
                          <form
                            className="stack"
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
                            <div className="form-grid">
                              <div className="col-span-12 sm:col-span-4 flex flex-col gap-1">
                                <label
                                  className="kicker"
                                  htmlFor={`reg-edit-name-${r.id}`}
                                >
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
                              <div className="col-span-12 sm:col-span-8 flex flex-col gap-1">
                                <div className="flex items-center gap-1.5">
                                  <label
                                    className="kicker"
                                    htmlFor={`reg-edit-endpoint-${r.id}`}
                                  >
                                    endpoint
                                  </label>
                                  <FieldHint
                                    id={`hint-reg-edit-endpoint-${r.id}`}
                                    label="about registry endpoint"
                                  >
                                    Registry hostname, e.g. <code>ghcr.io</code>{' '}
                                    or <code>registry.gitea.example</code>. Do
                                    not include the image path.
                                  </FieldHint>
                                </div>
                                <input
                                  id={`reg-edit-endpoint-${r.id}`}
                                  type="text"
                                  aria-label="edit registry endpoint"
                                  placeholder="ghcr.io"
                                  value={editEndpoint}
                                  onChange={(e) =>
                                    setEditEndpoint(e.target.value)
                                  }
                                  className="field-input w-full"
                                />
                              </div>
                              <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
                                <div className="flex items-center gap-1.5">
                                  <label
                                    className="kicker"
                                    htmlFor={`reg-edit-auth-${r.id}`}
                                  >
                                    auth kind
                                  </label>
                                  <FieldHint
                                    id={`hint-reg-edit-auth-${r.id}`}
                                    label="about auth kind"
                                  >
                                    <code>anonymous</code> public pulls;{' '}
                                    <code>basic</code> HTTP Basic (user/pass);{' '}
                                    <code>token</code> static bearer;{' '}
                                    <code>ecr_token</code> AWS ECR get-login
                                    flow; <code>gar_token</code> Google Artifact
                                    Registry.
                                  </FieldHint>
                                </div>
                                <select
                                  id={`reg-edit-auth-${r.id}`}
                                  aria-label="edit registry auth kind"
                                  value={editAuthKind}
                                  onChange={(e) =>
                                    setEditAuthKind(
                                      e.target
                                        .value as RegistryInput['auth_kind'],
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
                                  <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
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
                                  <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
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
                                      placeholder="leave blank to keep current"
                                      value={editPassword}
                                      onChange={(e) =>
                                        setEditPassword(e.target.value)
                                      }
                                      className="field-input w-full"
                                    />
                                  </div>
                                </>
                              ) : editAuthKind === 'anonymous' ? null : (
                                <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
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
                                    placeholder="leave blank to keep current"
                                    value={editToken}
                                    onChange={(e) =>
                                      setEditToken(e.target.value)
                                    }
                                    className="field-input w-full"
                                  />
                                </div>
                              )}
                            </div>
                            <p className="field-help">
                              Leave credential fields blank to keep the existing
                              secret.
                            </p>
                            <div className="cluster">
                              <button
                                type="submit"
                                className="btn btn-primary"
                                disabled={
                                  updateRegMutation.isPending ||
                                  editName.trim().length === 0 ||
                                  editEndpoint.trim().length === 0
                                }
                              >
                                {updateRegMutation.isPending ? 'saving' : 'save'}
                              </button>
                              <button
                                type="button"
                                className="btn"
                                onClick={() => {
                                  setRegEditId(null)
                                  setRegEditError('')
                                }}
                              >
                                cancel
                              </button>
                            </div>
                            {regEditError ? (
                              <InlineError message={regEditError} />
                            ) : null}
                          </form>
                        </td>
                      </tr>
                    ) : (
                      <tr key={r.id}>
                        <td style={{ fontWeight: 600 }}>{r.name}</td>
                        <td>
                          <Num className="text-faint">{r.endpoint}</Num>
                        </td>
                        <td>
                          <span className="badge">
                            {authKindLabel(r.auth_kind)}
                          </span>
                        </td>
                        <td>
                          {r.credential_ref ? (
                            <span className="cluster" style={{ gap: '0.3rem' }}>
                              <code style={{ fontSize: 'var(--text-label)' }}>
                                {r.credential_ref}
                              </code>
                              <CopyButton
                                value={r.credential_ref}
                                label="Copy credential reference"
                              />
                            </span>
                          ) : (
                            <span className="text-faint">—</span>
                          )}
                          {regDeleteErrorFor === r.id && regDeleteError ? (
                            <div style={{ marginTop: '0.4rem' }}>
                              <InlineError message={regDeleteError} />
                            </div>
                          ) : null}
                        </td>
                        <td className="num">
                          <span
                            className="cluster"
                            style={{ gap: '0.4rem', justifyContent: 'flex-end' }}
                          >
                            <button
                              type="button"
                              className="btn"
                              onClick={() => startEditRegistry(r)}
                            >
                              edit
                            </button>
                            <ConfirmButton
                              label="delete"
                              confirmLabel="Delete"
                              message={`Remove registry "${r.name}"?`}
                              busy={
                                deleteRegMutation.isPending &&
                                deleteRegMutation.variables === r.id
                              }
                              onConfirm={() => {
                                setRegDeleteError('')
                                setRegDeleteErrorFor(null)
                                deleteRegMutation.mutate(r.id)
                              }}
                              align="right"
                            />
                          </span>
                        </td>
                      </tr>
                    ),
                  )}
                </tbody>
              </table>
            </div>
          )}
        </section>

        <section>
          <form
            className="panel panel-pad stack"
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
            <p className="kicker m-0">add registry</p>
            <div className="form-grid">
              <div className="col-span-12 sm:col-span-4 flex flex-col gap-1">
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
              <div className="col-span-12 sm:col-span-8 flex flex-col gap-1">
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
              <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
                <div className="flex items-center gap-1.5">
                  <label className="kicker" htmlFor="reg-auth-kind">
                    auth kind
                  </label>
                  <FieldHint id="hint-reg-auth-kind" label="about auth kind">
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
                  <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
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
                  <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
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
                <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
                  <div className="flex items-center gap-1.5">
                    <label className="kicker" htmlFor="reg-token">
                      token
                    </label>
                    <FieldHint id="hint-reg-token" label="about token">
                      Encrypted server-side. The raw value is never stored or
                      shown again after you save it.
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
            <div className="cluster">
              <button
                type="submit"
                className="btn btn-primary"
                disabled={
                  createRegMutation.isPending ||
                  regName.trim().length === 0 ||
                  regEndpoint.trim().length === 0
                }
              >
                {createRegMutation.isPending ? 'creating' : 'add registry'}
              </button>
            </div>
            {regCreateError ? <InlineError message={regCreateError} /> : null}
          </form>
        </section>
      </div>
    </main>
  )
}
