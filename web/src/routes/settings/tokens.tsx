import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { Terminal } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useActionToasts } from '#/components/Toast'
import { ConfirmButton } from '#/components/ConfirmButton'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { CopyButton } from '#/components/CopyButton'
import { formatRelative } from '#/lib/format'

const listTokens = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listApiTokens
})

const createToken = (name: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createApiToken(name)
  })

const deleteToken = (id: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteApiToken(id)
  })

export const Route = createFileRoute('/settings/tokens')({
  component: SettingsTokens,
})

function Header() {
  return (
    <header style={{ marginBottom: '1.5rem' }}>
      <p className="kicker">settings</p>
      <h1 className="t-display">API tokens</h1>
      <p className="text-faint" style={{ marginTop: 6, maxWidth: '60ch' }}>
        Long-lived bearer tokens for the CLI and automation. The secret is shown
        once at creation; revoke a token to invalidate it immediately.
      </p>
    </header>
  )
}

function SettingsTokens() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const [name, setName] = useState('')
  const [revealedToken, setRevealedToken] = useState<string | null>(null)

  const {
    data: tokens = [],
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['api-tokens'],
    queryFn: () => runQuery(listTokens),
    enabled: !auth.isBootstrap,
  })

  const createMut = useMutation({
    mutationFn: (tokenName: string) => runQuery(createToken(tokenName)),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['api-tokens'] })
      setRevealedToken(data.token)
      setName('')
      toast.ok('Token minted.')
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  const deleteMut = useMutation({
    mutationFn: (id: string) => runQuery(deleteToken(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-tokens'] })
      toast.ok('Token revoked.')
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  if (auth.isBootstrap) {
    return (
      <div className="page-narrow px-4 pb-16 pt-10">
        <Header />
        <div className="panel">
          <EmptyState
            icon={<Terminal size={22} />}
            title="Not available for bootstrap"
            hint="API tokens are not available for bootstrap principals. Create a super-admin user and sign in to mint tokens."
          />
        </div>
      </div>
    )
  }

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <Header />

      <div className="stack">
        {revealedToken ? (
          <section
            className="panel panel-pad"
            role="status"
            style={{
              borderColor:
                'color-mix(in oklab, var(--pink) 45%, var(--border))',
            }}
          >
            <div className="panel-head">
              <p className="kicker">new token</p>
              <button
                type="button"
                className="btn"
                onClick={() => setRevealedToken(null)}
              >
                Dismiss
              </button>
            </div>
            <p className="field-error" style={{ marginTop: 0 }}>
              Copy this token now. It will not be shown again.
            </p>
            <div
              className="cluster"
              style={{ marginTop: '0.75rem', flexWrap: 'nowrap' }}
            >
              <code
                className="tnum"
                style={{
                  flex: 1,
                  minWidth: 0,
                  overflowWrap: 'anywhere',
                  padding: '0.5rem 0.6rem',
                  background: 'var(--surface-2)',
                }}
              >
                {revealedToken}
              </code>
              <CopyButton value={revealedToken} label="Copy token" />
            </div>
          </section>
        ) : null}

        <form
          onSubmit={(e) => {
            e.preventDefault()
            if (!name.trim()) return
            createMut.mutate(name.trim())
          }}
          className="panel panel-pad"
        >
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            mint token
          </p>
          <div className="flex flex-wrap items-end gap-3">
            <div className="flex flex-col gap-1.5">
              <label htmlFor="token-name" className="kicker">
                token name
              </label>
              <input
                id="token-name"
                name="token-name"
                type="text"
                placeholder="ci-deploy"
                value={name}
                onChange={(e) => setName(e.target.value)}
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
                  <span className="spin" aria-hidden="true" /> Minting
                </>
              ) : (
                'Mint token'
              )}
            </button>
          </div>
        </form>

        <section>
          <p className="kicker" style={{ marginBottom: '0.9rem' }}>
            tokens
          </p>
          {isLoading ? (
            <SkeletonRows rows={3} />
          ) : error ? (
            <ErrorPanel
              message={errorMessage(error)}
              onRetry={() => void refetch()}
            />
          ) : tokens.length === 0 ? (
            <div className="panel">
              <EmptyState
                icon={<Terminal size={22} />}
                title="No tokens yet"
                hint="Mint a token above to authenticate the CLI or CI pipelines."
              />
            </div>
          ) : (
            <div className="panel overflow-hidden">
              <table className="dtable">
                <thead>
                  <tr>
                    <th>name</th>
                    <th>created</th>
                    <th className="num">actions</th>
                  </tr>
                </thead>
                <tbody>
                  {tokens.map((t) => (
                    <tr key={t.id}>
                      <td>{t.name}</td>
                      <td className="text-faint tnum">
                        {formatRelative(t.created_at, Date.now())}
                      </td>
                      <td className="num">
                        <ConfirmButton
                          label="Revoke"
                          confirmLabel="Revoke token"
                          message={`Revoke "${t.name}"? Any client using it will be rejected immediately.`}
                          onConfirm={() => deleteMut.mutate(t.id)}
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
