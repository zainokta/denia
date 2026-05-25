import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { useState } from 'react'

const listTokens = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listApiTokens
})

const createToken = (name: string) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createApiToken(name)
  })

const deleteToken = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.deleteApiToken(id)
  })

export const Route = createFileRoute('/settings/tokens')({
  component: SettingsTokens,
})

function SettingsTokens() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [revealedToken, setRevealedToken] = useState<string | null>(null)

  const { data: tokens = [], isFetching } = useQuery({
    queryKey: ['api-tokens'],
    queryFn: () => runQuery(listTokens),
  })

  const createMut = useMutation({
    mutationFn: (tokenName: string) => runQuery(createToken(tokenName)),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['api-tokens'] })
      setRevealedToken(data.token)
      setName('')
    },
  })

  const deleteMut = useMutation({
    mutationFn: (id: number) => runQuery(deleteToken(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-tokens'] })
    },
  })

  if (auth.isBootstrap) {
    return (
      <main className="page-wrap px-4 py-12">
        <p className="text-sm text-[var(--fg-muted)]">
          API tokens are not available for bootstrap principals. Create a
          super-admin user and sign in to mint tokens.
        </p>
      </main>
    )
  }

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">settings</p>
      <h1 className="mb-6 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        API Tokens
      </h1>

      {revealedToken ? (
        <div className="panel mb-8 p-4 space-y-2">
          <p className="kicker">token created</p>
          <p className="text-sm text-[var(--violet)]">
            Copy this token now. It will not be shown again.
          </p>
          <code className="block break-all p-2 text-xs bg-[var(--surface-2)] rounded">
            {revealedToken}
          </code>
          <button
            type="button"
            className="btn text-xs"
            onClick={() => setRevealedToken(null)}
          >
            dismiss
          </button>
        </div>
      ) : null}

      <form
        onSubmit={(e) => {
          e.preventDefault()
          createMut.mutate(name)
        }}
        className="panel mb-8 p-4 space-y-3"
      >
        <p className="kicker mb-2">mint token</p>
        <div className="flex flex-wrap gap-3">
          <input
            type="text"
            placeholder="token name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            className="bg-[var(--bg)] border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--fg)] focus:outline-none focus:border-[var(--pink)]"
            required
          />
          <button
            type="submit"
            className="btn btn-primary text-xs"
            disabled={createMut.isPending}
          >
            {createMut.isPending ? 'minting...' : 'mint'}
          </button>
        </div>
      </form>

      <section className="panel overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          {isFetching
            ? 'fetching...'
            : `${tokens.length} token${tokens.length !== 1 ? 's' : ''}`}
        </p>
        <ul className="m-0 list-none">
          {tokens.map((t, i) => (
            <li
              key={t.id}
              className={`flex items-center gap-4 px-4 py-3 text-sm ${
                i > 0 ? 'border-t border-[var(--border)]' : ''
              }`}
            >
              <span className="flex-1 text-[var(--fg)]">{t.name}</span>
              <span className="text-xs text-[var(--fg-muted)]">
                {t.created_at}
              </span>
              <button
                type="button"
                className="btn text-xs"
                onClick={() => deleteMut.mutate(t.id)}
                disabled={deleteMut.isPending}
              >
                revoke
              </button>
            </li>
          ))}
        </ul>
      </section>
    </main>
  )
}
