import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { MonitorSmartphone } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useActionToasts } from '#/components/Toast'
import { ConfirmButton } from '#/components/ConfirmButton'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { formatRelative, shortId } from '#/lib/format'

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

function Header() {
  return (
    <header style={{ marginBottom: '1.5rem' }}>
      <p className="kicker">settings</p>
      <h1 className="t-display">Sessions</h1>
      <p className="text-faint" style={{ marginTop: 6, maxWidth: '70ch' }}>
        Active sign-in sessions for your account. Revoking all sessions ends
        every browser and CLI you are signed into and logs you out here.
      </p>
    </header>
  )
}

function SettingsSessions() {
  const auth = useAuth()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const toast = useActionToasts()

  const {
    data: sessions = [],
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['sessions'],
    queryFn: () => runQuery(listSessions),
    enabled: !!auth.token,
  })

  const revokeMut = useMutation({
    mutationFn: () => runQuery(revokeAll),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
      toast.ok('All sessions revoked.')
      navigate({ to: '/login' })
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  if (!auth.token) {
    return (
      <div className="page-narrow px-4 pb-16 pt-10">
        <Header />
        <div className="panel">
          <EmptyState
            icon={<MonitorSmartphone size={22} />}
            title="Not signed in"
            hint="Sign in to view and manage your active sessions."
          />
        </div>
      </div>
    )
  }

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <Header />

      <div className="stack">
        <section>
          <div className="panel-head">
            <p className="kicker">active sessions</p>
            <ConfirmButton
              label={revokeMut.isPending ? 'Revoking…' : 'Sign out everywhere'}
              confirmLabel="Revoke all"
              message="Sign out everywhere? Every browser and CLI session ends and you will be redirected to login."
              onConfirm={() => revokeMut.mutate()}
              busy={revokeMut.isPending}
              disabled={sessions.length === 0}
              align="right"
            />
          </div>
          {isLoading ? (
            <SkeletonRows rows={3} />
          ) : error ? (
            <ErrorPanel
              message={errorMessage(error)}
              onRetry={() => void refetch()}
            />
          ) : sessions.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<MonitorSmartphone size={22} />}
                title="No active sessions"
                hint="You have no other active sign-in sessions."
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>session</th>
                    <th>expires</th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.map((s) => (
                    <tr key={s.id}>
                      <td>
                        <code>{shortId(s.id)}</code>
                      </td>
                      <td className="text-faint tnum">
                        {formatRelative(s.expires_at, Date.now())}
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
