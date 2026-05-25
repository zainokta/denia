import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useAuth } from '../hooks/useAuth'

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

  return (
    <main className="page-wrap px-4 py-16">
      <div className="mx-auto max-w-sm">
        <p className="kicker mb-3">auth</p>
        <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
          sign in
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
          {error && (
            <p className="text-sm text-[var(--violet)]">{error}</p>
          )}
          <button
            type="submit"
            className="btn btn-primary w-full justify-center"
            disabled={auth.loginStatus === 'pending'}
          >
            {auth.loginStatus === 'pending' ? 'signing in...' : 'sign in'}
          </button>
        </form>
      </div>
    </main>
  )
}
