import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { LogIn } from 'lucide-react'
import { useAuth } from '../hooks/useAuth'
import { InlineError } from '#/components/ErrorPanel'

export const Route = createFileRoute('/login')({
  component: Login,
})

export function Login() {
  const navigate = useNavigate()
  const auth = useAuth()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    auth.login({ username, password }).then(
      () => navigate({ to: '/' }),
      (err: unknown) =>
        setError(err instanceof Error ? err.message : 'Invalid credentials'),
    )
  }

  const pending = auth.loginStatus === 'pending'

  return (
    <main className="page-narrow px-4 py-16">
      <header style={{ marginBottom: '1.5rem' }}>
        <p className="kicker">denia control</p>
        <h1 className="t-display">Sign in</h1>
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
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
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
                <span className="spin" aria-hidden="true" /> Signing in
              </>
            ) : (
              <>
                <LogIn size={14} aria-hidden="true" /> Sign in
              </>
            )}
          </button>
        </div>
      </form>
    </main>
  )
}
