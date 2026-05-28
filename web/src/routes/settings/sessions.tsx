import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'

const listSessions = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listSessions
})

const revokeAll = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.revokeAllSessions
})

export const Route = createFileRoute('/settings/sessions')({
  component: SettingsSessions,
})

function SettingsSessions() {
  const auth = useAuth()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [confirm, setConfirm] = useState(false)
  const [error, setError] = useState('')

  const { data: sessions = [], isFetching } = useQuery({
    queryKey: ['sessions'],
    queryFn: () => runQuery(listSessions),
    enabled: !!auth.token,
  })

  const revokeMut = useMutation({
    mutationFn: () => runQuery(revokeAll),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
      setConfirm(false)
      navigate({ to: '/login' })
    },
    onError: (err: unknown) => {
      setError(err instanceof Error ? err.message : 'Failed to revoke')
      setConfirm(false)
    },
  })

  if (!auth.token) {
    return (
      <main className="page-wrap px-4 py-12">
        <p className="text-[var(--violet)]">Not signed in.</p>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">settings</p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Sessions
      </h1>

      <p className="mb-6 max-w-[70ch] text-sm text-[var(--fg-muted)]">
        Active sign-in sessions for your account. Revoking all sessions ends
        every browser and CLI you are signed into and logs you out here.
      </p>

      <section className="panel mb-6 overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          {isFetching
            ? 'fetching...'
            : `${sessions.length} session${sessions.length !== 1 ? 's' : ''}`}
        </p>
        {sessions.length === 0 ? (
          <p className="px-4 py-3 text-sm text-[var(--fg-muted)]">
            No active sessions.
          </p>
        ) : (
          <ul className="m-0 list-none">
            {sessions.map((s, i) => (
              <li
                key={s.id}
                className={`flex flex-wrap items-baseline gap-x-4 gap-y-1 px-4 py-3 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span className="font-mono text-[var(--fg)]">{s.id}…</span>
                <span className="tnum text-xs text-[var(--fg-muted)]">
                  expires {s.expires_at}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        {error ? (
          <p
            role="alert"
            className="mb-2 text-xs text-[var(--violet)]"
            aria-live="polite"
          >
            <span className="signal signal-fault mr-2 inline-block align-middle" />
            {error}
          </p>
        ) : null}
        {confirm ? (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm text-[var(--violet)]">
              Sign out everywhere? You will be redirected to login.
            </span>
            <button
              type="button"
              className="btn btn-danger text-xs"
              onClick={() => {
                setError('')
                revokeMut.mutate()
              }}
              disabled={revokeMut.isPending}
            >
              {revokeMut.isPending ? 'revoking...' : 'confirm revoke all'}
            </button>
            <button
              type="button"
              className="btn text-xs"
              onClick={() => setConfirm(false)}
              disabled={revokeMut.isPending}
            >
              cancel
            </button>
          </div>
        ) : (
          <button
            type="button"
            className="btn btn-danger text-xs"
            onClick={() => {
              setError('')
              setConfirm(true)
            }}
            disabled={sessions.length === 0}
          >
            sign out everywhere
          </button>
        )}
      </section>
    </main>
  )
}
