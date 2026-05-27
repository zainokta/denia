import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { clearToken } from '../effect/auth-store'
import { runQuery } from '../effect/runtime'
import { ApiClient } from '../effect/api-client'

export const Route = createFileRoute('/setup')({
  component: Setup,
})

function statusOf(err: unknown): number | undefined {
  if (typeof err === 'object' && err !== null && 'status' in err) {
    const status = err.status
    return typeof status === 'number' ? status : undefined
  }
  return undefined
}

export function Setup() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [error, setError] = useState('')

  const bootstrapMutation = useMutation({
    mutationFn: (creds: { username: string; password: string }) =>
      runQuery(
        Effect.gen(function* () {
          const api = yield* ApiClient
          return yield* api.bootstrap(creds.username, creds.password)
        }),
      ),
    onSuccess: () => {
      clearToken()
      queryClient.removeQueries({ queryKey: ['me'] })
      navigate({ to: '/login' })
    },
    onError: (err: unknown) => {
      const status = statusOf(err)
      if (status === 409) {
        clearToken()
        queryClient.removeQueries({ queryKey: ['me'] })
        navigate({ to: '/login' })
        return
      }
      if (status === 401) {
        setError('missing or invalid admin token')
        return
      }
      if (status === 400) {
        setError(err instanceof Error ? err.message : 'invalid request')
        return
      }
      setError('could not create the first admin')
    },
  })

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    if (password !== confirm) {
      setError('passwords do not match')
      return
    }
    bootstrapMutation.mutate({ username, password })
  }

  return (
    <main className="page-wrap px-4 py-16">
      <div className="mx-auto max-w-sm">
        <p className="kicker mb-3">setup</p>
        <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
          create first admin
        </h1>
        <form onSubmit={handleSubmit} className="panel p-6 space-y-4">
          <div>
            <label htmlFor="username" className="kicker block mb-1">
              username
            </label>
            <input
              id="username"
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
              required
            />
          </div>
          <div>
            <label htmlFor="password" className="kicker block mb-1">
              password
            </label>
            <input
              id="password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
              required
            />
          </div>
          <div>
            <label htmlFor="confirm" className="kicker block mb-1">
              confirm password
            </label>
            <input
              id="confirm"
              type="password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              className="w-full bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
              required
            />
          </div>
          {error && <p className="text-sm text-[var(--violet)]">{error}</p>}
          <button
            type="submit"
            className="btn btn-primary w-full justify-center"
            disabled={bootstrapMutation.status === 'pending'}
          >
            {bootstrapMutation.status === 'pending'
              ? 'creating...'
              : 'create admin'}
          </button>
        </form>
      </div>
    </main>
  )
}
