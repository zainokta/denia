import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { Users } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useActionToasts } from '#/components/Toast'
import { ConfirmButton } from '#/components/ConfirmButton'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { formatRelative } from '#/lib/format'

const listUsers = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listUsers
})

const createUser = (input: { username: string; password: string }) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createUser(input.username, input.password)
  })

const deleteUser = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteUser(id)
  })

export const Route = createFileRoute('/settings/users')({
  component: SettingsUsers,
})

function Header() {
  return (
    <header style={{ marginBottom: '1.5rem' }}>
      <p className="kicker">settings</p>
      <h1 className="t-display">Users</h1>
      <p className="text-faint" style={{ marginTop: 6, maxWidth: '60ch' }}>
        Local accounts that can sign in to the control plane. Super-admins
        manage who has access here.
      </p>
    </header>
  )
}

function SettingsUsers() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')

  const {
    data: users = [],
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['users'],
    queryFn: () => runQuery(listUsers),
    enabled: auth.isSuperAdmin,
  })

  const createMut = useMutation({
    mutationFn: (input: { username: string; password: string }) =>
      runQuery(createUser(input)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['users'] })
      setUsername('')
      setPassword('')
      toast.ok('User created.')
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  const deleteMut = useMutation({
    mutationFn: (id: string) => runQuery(deleteUser(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['users'] })
      toast.ok('User deleted.')
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  if (!auth.isSuperAdmin) {
    return (
      <div className="page-narrow px-4 pb-16 pt-10">
        <Header />
        <div className="panel">
          <EmptyState
            icon={<Users size={22} />}
            title="Super-admin only"
            hint="Managing user accounts is restricted to super-admins."
          />
        </div>
      </div>
    )
  }

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <Header />

      <div className="stack">
        <form
          onSubmit={(e) => {
            e.preventDefault()
            if (!username.trim() || !password) return
            createMut.mutate({ username: username.trim(), password })
          }}
          className="panel panel-pad"
        >
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            create user
          </p>
          <div className="flex flex-wrap items-end gap-3">
            <div className="flex flex-col gap-1.5">
              <label htmlFor="new-username" className="kicker">
                username
              </label>
              <input
                id="new-username"
                name="username"
                type="text"
                autoComplete="off"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="field-input"
                required
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <label htmlFor="new-password" className="kicker">
                password
              </label>
              <input
                id="new-password"
                name="password"
                type="password"
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="field-input"
                required
              />
            </div>
            <button
              type="submit"
              className="btn btn-primary"
              disabled={createMut.isPending}
            >
              {createMut.isPending ? (
                <>
                  <span className="spin" aria-hidden="true" /> Creating
                </>
              ) : (
                'Create user'
              )}
            </button>
          </div>
        </form>

        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            accounts
          </p>
          {isLoading ? (
            <SkeletonRows rows={3} />
          ) : error ? (
            <ErrorPanel
              message={errorMessage(error)}
              onRetry={() => void refetch()}
            />
          ) : users.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Users size={22} />}
                title="No users yet"
                hint="Create the first additional account using the form above."
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>username</th>
                    <th>created</th>
                    <th className="num">actions</th>
                  </tr>
                </thead>
                <tbody>
                  {users.map((u) => (
                    <tr key={u.id}>
                      <td>{u.username}</td>
                      <td className="text-faint tnum">
                        {formatRelative(u.created_at, Date.now())}
                      </td>
                      <td className="num">
                        <ConfirmButton
                          label="Delete"
                          confirmLabel="Delete user"
                          message={`Delete the account "${u.username}"? This cannot be undone.`}
                          onConfirm={() => deleteMut.mutate(u.id)}
                          busy={deleteMut.isPending}
                          align="right"
                        />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </div>
  )
}
