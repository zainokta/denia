import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useAuth } from '#/hooks/useAuth'
import { FieldHint } from '#/components/FieldHint'
import type { CredentialInput, CredentialKind } from '#/effect/schema'

const listCredentials = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listCredentials
})

const putCredential = (input: CredentialInput) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.putCredential(input)
  })

const KIND_OPTIONS: ReadonlyArray<[CredentialKind, string]> = [
  ['SshDeployKey', 'SSH Deploy Key (git)'],
  ['RegistryBasic', 'Registry Basic Auth'],
  ['RegistryToken', 'Registry Token'],
]

const kindLabel = (k: CredentialKind) =>
  KIND_OPTIONS.find(([v]) => v === k)?.[1] ?? k

export const Route = createFileRoute('/settings/credentials')({
  component: SettingsCredentials,
})

function SettingsCredentials() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [kind, setKind] = useState<CredentialKind>('SshDeployKey')
  const [secretRef, setSecretRef] = useState('')
  const [error, setError] = useState('')

  const { data: credentials = [], isFetching } = useQuery({
    queryKey: ['credentials'],
    queryFn: () => runQuery(listCredentials),
    enabled: auth.isSuperAdmin,
  })

  const createMut = useMutation({
    mutationFn: (input: CredentialInput) => runQuery(putCredential(input)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      setName('')
      setSecretRef('')
      setError('')
    },
    onError: (err: unknown) => {
      setError(err instanceof Error ? err.message : 'Failed')
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
        Credentials
      </h1>

      <p className="mb-6 max-w-[70ch] text-sm text-[var(--fg-muted)]">
        Credentials reference a SOPS-encrypted file under{' '}
        <code>secrets/&lt;project&gt;/</code>. Create the encrypted file out of
        band, then register a credential here so service forms and registry
        configs can reference it by name.
      </p>

      <form
        onSubmit={(e) => {
          e.preventDefault()
          setError('')
          if (!name.trim() || !secretRef.trim()) return
          createMut.mutate({
            name: name.trim(),
            kind,
            secret_ref: secretRef.trim(),
          })
        }}
        className="panel mb-8 p-4"
      >
        <p className="kicker mb-4">register credential</p>
        <div className="form-grid mb-3">
          <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
            <label className="kicker" htmlFor="cred-name">
              name
            </label>
            <input
              id="cred-name"
              type="text"
              placeholder="ghcr-token"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="field-input w-full"
              required
            />
          </div>
          <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
            <div className="flex items-center gap-1.5">
              <label className="kicker" htmlFor="cred-kind">
                kind
              </label>
              <FieldHint id="hint-cred-kind" label="about credential kind">
                <code>SshDeployKey</code> for git clones;{' '}
                <code>RegistryBasic</code> for HTTP Basic registries;{' '}
                <code>RegistryToken</code> for bearer-token registries (GHCR
                PAT, ECR, GAR).
              </FieldHint>
            </div>
            <select
              id="cred-kind"
              value={kind}
              onChange={(e) => setKind(e.target.value as CredentialKind)}
              className="field-input w-full"
            >
              {KIND_OPTIONS.map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </div>
          <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
            <div className="flex items-center gap-1.5">
              <label className="kicker" htmlFor="cred-secret">
                secret ref
              </label>
              <FieldHint id="hint-cred-secret" label="about secret ref">
                Filename stem of the SOPS file under{' '}
                <code>&lt;data&gt;/secrets/&lt;project&gt;/</code>. Denia
                resolves <code>{'<ref>'}.sops.yaml</code> at use time.
              </FieldHint>
            </div>
            <input
              id="cred-secret"
              type="text"
              placeholder="ghcr-token"
              value={secretRef}
              onChange={(e) => setSecretRef(e.target.value)}
              className="field-input w-full"
              required
            />
          </div>
        </div>
        <button
          type="submit"
          className="btn btn-primary text-xs"
          disabled={createMut.isPending}
        >
          {createMut.isPending ? 'saving...' : 'register'}
        </button>
        {error ? (
          <p className="mt-2 text-sm text-[var(--violet)]">{error}</p>
        ) : null}
      </form>

      <section className="panel overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          {isFetching
            ? 'fetching...'
            : `${credentials.length} credential${credentials.length !== 1 ? 's' : ''}`}
        </p>
        {credentials.length === 0 ? (
          <p className="px-4 py-3 text-sm text-[var(--fg-muted)]">
            No credentials registered.
          </p>
        ) : (
          <ul className="m-0 list-none">
            {credentials.map((c, i) => (
              <li
                key={c.id}
                className={`flex flex-wrap items-baseline gap-x-4 gap-y-1 px-4 py-3 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                <span className="font-semibold text-[var(--fg)]">{c.name}</span>
                <span className="kicker">{kindLabel(c.kind)}</span>
                <span className="text-xs text-[var(--fg-muted)]">
                  secret: {c.secret_ref}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </main>
  )
}
