import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useState } from 'react'

const listUsers = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listUsers
})

const createUser = (input: { username: string; password: string }) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createUser(input.username, input.password)
  })

const deleteUser = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteUser(id)
  })

export const Route = createFileRoute('/settings/users')({
  component: SettingsUsers,
})

function SettingsUsers() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')

  const { data: users = [], isFetching } = useQuery({
    queryKey: ['users'],
    queryFn: () => runQuery(listUsers),
  })

  const createMut = useMutation({
    mutationFn: (input: { username: string; password: string }) =>
      runQuery(createUser(input)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['users'] })
      setUsername('')
      setPassword('')
    },
    onError: (err: unknown) => {
      setError(err instanceof Error ? err.message : 'Failed')
    },
  })

  const deleteMut = useMutation({
    mutationFn: (id: number) => runQuery(deleteUser(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['users'] })
    },
  })

  if (!auth.isSuperAdmin) {
    return (
      <main className="page-wrap px-4 py-12">
        <p className="text-[var(--violet)]">Access denied.</p>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">settings</p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Users
      </h1>

      <form
        onSubmit={(e) => {
          e.preventDefault()
          setError('')
          createMut.mutate({ username, password })
        }}
        className="panel mb-8 p-4 space-y-3"
      >
        <p className="kicker mb-2">create user</p>
        <div className="flex flex-wrap gap-3">
          <input
            type="text"
            placeholder="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            className="bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
            required
          />
          <input
            type="password"
            placeholder="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className="bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
            required
          />
          <button
            type="submit"
            className="btn btn-primary text-xs"
            disabled={createMut.isPending}
          >
            {createMut.isPending ? 'creating...' : 'create'}
          </button>
        </div>
        {error ? (
          <p className="text-sm text-[var(--violet)]">{error}</p>
        ) : null}
      </form>

      <section className="panel overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          {isFetching
            ? 'fetching...'
            : `${users.length} user${users.length !== 1 ? 's' : ''}`}
        </p>
        <ul className="m-0 list-none">
          {users.map((u, i) => (
            <li
              key={u.id}
              className={`flex items-center gap-4 px-4 py-3 text-sm ${
                i > 0 ? 'border-t border-[var(--border)]' : ''
              }`}
            >
              <span className="flex-1 text-[var(--fg)]">{u.username}</span>
              <span className="text-xs text-[var(--fg-muted)]">
                {u.created_at}
              </span>
              <button
                type="button"
                className="btn text-xs"
                onClick={() => deleteMut.mutate(u.id)}
                disabled={deleteMut.isPending}
              >
                delete
              </button>
            </li>
          ))}
        </ul>
      </section>
    </main>
  )
}
