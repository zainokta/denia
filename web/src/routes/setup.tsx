import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ShieldCheck } from 'lucide-react'
import { clearToken } from '../effect/auth-store'
import { runQuery } from '../effect/runtime'
import { ApiClient } from '../effect/api-client'
import { InlineError } from '#/components/ErrorPanel'

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

  const pending = bootstrapMutation.status === 'pending'

  return (
    <main className="page-narrow px-4 py-16">
      <header style={{ marginBottom: '1.5rem' }}>
        <p className="kicker">denia setup</p>
        <h1 className="t-display">Create admin</h1>
        <p className="text-faint" style={{ marginTop: 6, maxWidth: '52ch' }}>
          Bootstrap the first super-admin account for this node. After it is
          created you will be redirected to sign in.
        </p>
      </header>

      <form onSubmit={handleSubmit} className="panel panel-pad">
        <div className="stack">
          <div className="flex flex-col gap-1.5">
            <label htmlFor="username" className="kicker">
              username
            </label>
            <input
              id="username"
              name="username"
              type="text"
              autoComplete="username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="field-input w-full"
              required
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor="password" className="kicker">
              password
            </label>
            <input
              id="password"
              name="password"
              type="password"
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="field-input w-full"
              required
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor="confirm" className="kicker">
              confirm password
            </label>
            <input
              id="confirm"
              name="confirm"
              type="password"
              autoComplete="new-password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              className="field-input w-full"
              required
            />
          </div>
          {error ? <InlineError message={error} /> : null}
          <button
            type="submit"
            className="btn btn-primary w-full justify-center"
            disabled={pending}
          >
            {pending ? (
              <>
                <span className="spin" aria-hidden="true" /> Creating
              </>
            ) : (
              <>
                <ShieldCheck size={14} aria-hidden="true" /> Create admin
              </>
            )}
          </button>
        </div>
      </form>
    </main>
  )
}
