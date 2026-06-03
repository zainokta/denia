import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { Boxes, Terminal } from 'lucide-react'
import { useState } from 'react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { HostedRegistryStatus, HostedRepository } from '#/effect/schema'
import { useAuth } from '#/hooks/useAuth'
import { useActionToasts } from '#/components/Toast'
import { ConfirmButton } from '#/components/ConfirmButton'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { Num } from '#/components/Num'
import { formatBytes, formatRelative } from '#/lib/format'
import { Modal } from '#/components/Modal'
import { CopyButton } from '#/components/CopyButton'

const getHostedRegistryStatus = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getHostedRegistryStatus
})

const listHostedRepositories = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listHostedRepositories
})

const runGc = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.runHostedRegistryGc
})

export const Route = createFileRoute('/settings/hosted-registry')({
  component: HostedRegistryPage,
})

function HostedRegistryPage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const toast = useActionToasts()

  const statusQuery = useQuery({
    queryKey: ['registry', 'status'],
    queryFn: () => runQuery(getHostedRegistryStatus),
    enabled: auth.isSuperAdmin,
    retry: false,
  })

  const reposQuery = useQuery({
    queryKey: ['registry', 'repositories'],
    queryFn: () => runQuery(listHostedRepositories),
    retry: false,
  })

  const gcMut = useMutation({
    mutationFn: () => runQuery(runGc),
    onSuccess: (result) => {
      queryClient.setQueryData(['registry', 'status'], result)
      toast.ok(`GC reclaimed ${formatBytes(result.last_gc_deleted_bytes)}.`)
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <Header />

      {auth.isSuperAdmin ? (
        <section className="panel panel-pad" style={{ marginBottom: '1.5rem' }}>
          <div className="panel-head">
            <p className="kicker">registry status</p>
            <GcButton
              busy={gcMut.isPending}
              onConfirm={() => gcMut.mutate()}
            />
          </div>

          {statusQuery.isLoading ? (
            <SkeletonRows rows={2} />
          ) : statusQuery.error ? (
            <ErrorPanel
              message={errorMessage(statusQuery.error)}
              onRetry={() => void statusQuery.refetch()}
            />
          ) : statusQuery.data ? (
            <StatusGrid status={statusQuery.data} />
          ) : null}
        </section>
      ) : null}

      <section className="panel">
        <div className="panel-head panel-pad" style={{ borderBottom: '1px solid var(--border)' }}>
          <p className="kicker">repositories</p>
        </div>

        {reposQuery.isLoading ? (
          <div className="panel-pad">
            <SkeletonRows rows={4} />
          </div>
        ) : reposQuery.error ? (
          <div className="panel-pad">
            <ErrorPanel
              message={errorMessage(reposQuery.error)}
              onRetry={() => void reposQuery.refetch()}
            />
          </div>
        ) : reposQuery.data ? (
          <RepositoriesTable repositories={reposQuery.data} />
        ) : null}
      </section>
    </div>
  )
}

function Header() {
  return (
    <header style={{ marginBottom: '1.5rem' }}>
      <p className="kicker">settings</p>
      <h1 className="t-display">Hosted registry</h1>
      <p className="text-faint" style={{ marginTop: 6, maxWidth: '60ch' }}>
        Denia-hosted OCI images served under <code>/v2</code>, stored locally,
        and garbage-collected to reclaim unreferenced blobs.
      </p>
    </header>
  )
}

function StatusGrid({ status }: { readonly status: HostedRegistryStatus }) {
  return (
    <div className="grid gap-6" style={{ gridTemplateColumns: 'repeat(auto-fit, minmax(9rem, 1fr))' }}>
      <Stat label="repositories" value={<Num>{status.repositories.toLocaleString('en-US')}</Num>} />
      <Stat label="blobs" value={<Num>{status.blobs.toLocaleString('en-US')}</Num>} />
      <Stat label="total size" value={<Num>{formatBytes(status.total_bytes)}</Num>} />
      <Stat
        label="last GC"
        value={
          <span className="tnum">
            {status.last_gc_at ? formatRelative(status.last_gc_at, Date.now()) : 'never'}
          </span>
        }
        sub={
          status.last_gc_at
            ? `reclaimed ${formatBytes(status.last_gc_deleted_bytes)}`
            : undefined
        }
      />
    </div>
  )
}

// Derive the registry host from the current window location (no scheme — docker registry form).
// Falls back to a placeholder in SSR / test environments where window is absent.
function registryHost(): string {
  return typeof window !== 'undefined' && window.location.host
    ? window.location.host
    : 'denia.example.com'
}

// Small helper: one labelled command row with a copy button.
function CommandLine({ label, command }: { readonly label: string; readonly command: string }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '0.3rem' }}>
      <span className="kicker">{label}</span>
      <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
        <code
          style={{
            flex: 1,
            display: 'block',
            fontFamily: 'var(--font-mono)',
            fontSize: 'var(--text-body)',
            background: 'var(--surface-2)',
            border: '1px solid var(--border)',
            borderRadius: '6px',
            padding: '0.5rem 0.75rem',
            overflowX: 'auto',
            whiteSpace: 'pre',
            lineHeight: 1.5,
          }}
        >
          {command}
        </code>
        <CopyButton value={command} />
      </div>
    </div>
  )
}

// Exported for tests.
export function PushCommandsModal({
  repository,
  open,
  onClose,
}: {
  readonly repository: string
  readonly open: boolean
  readonly onClose: () => void
}) {
  const host = registryHost()
  const login = `docker login ${host} -u denia -p <API_TOKEN>`
  const tag = `docker tag <local-image>:latest ${host}/${repository}:latest`
  const push = `docker push ${host}/${repository}:latest`
  const pull = `docker pull ${host}/${repository}:latest`

  return (
    <Modal open={open} onClose={onClose} title={`Push commands — ${repository}`}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: '1.1rem' }}>
        <p
          className="text-faint"
          style={{ fontSize: 'var(--text-body)', margin: 0, lineHeight: 1.55 }}
        >
          Authenticate with any username and a Denia API token as the password
          (create one under Settings → API tokens).
        </p>

        <CommandLine label="login" command={login} />
        <CommandLine label="tag" command={tag} />
        <CommandLine label="push" command={push} />
        <CommandLine label="pull" command={pull} />

        <p
          className="text-faint"
          style={{ fontSize: 'var(--text-label)', margin: 0, lineHeight: 1.55 }}
        >
          For a non-HTTPS host, docker needs the registry in its insecure-registries list.
        </p>
      </div>
    </Modal>
  )
}

// Exported for tests: renders the repository table (or empty state).
export function RepositoriesTable({
  repositories,
}: {
  readonly repositories: ReadonlyArray<HostedRepository>
}) {
  if (repositories.length === 0) {
    return (
      <EmptyState
        icon={<Boxes size={22} />}
        title="No repositories yet"
        hint="Push an image to the hosted registry to see repositories here."
      />
    )
  }

  return (
    <div style={{ overflowX: 'auto' }}>
      <table className="data-table" style={{ width: '100%' }}>
        <thead>
          <tr>
            <th className="kicker" style={{ paddingInline: '1rem', paddingBlock: '0.6rem', textAlign: 'left' }}>repository</th>
            <th className="kicker" style={{ paddingInline: '1rem', paddingBlock: '0.6rem', textAlign: 'left' }}>project</th>
            <th className="kicker" style={{ paddingInline: '1rem', paddingBlock: '0.6rem', textAlign: 'left' }}>service</th>
            <th className="kicker" style={{ paddingInline: '1rem', paddingBlock: '0.6rem', textAlign: 'left' }}>tags</th>
            <th className="kicker" style={{ paddingInline: '1rem', paddingBlock: '0.6rem', textAlign: 'right' }}></th>
          </tr>
        </thead>
        <tbody>
          {repositories.map((repo) => (
            <RepositoryRow key={repo.repository} repo={repo} />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function RepositoryRow({ repo }: { readonly repo: HostedRepository }) {
  const [open, setOpen] = useState(false)

  return (
    <tr style={{ borderTop: '1px solid var(--border)' }}>
      <td
        className="tnum"
        style={{
          paddingInline: '1rem',
          paddingBlock: '0.75rem',
          fontFamily: 'var(--font-mono)',
          fontSize: 'var(--text-body)',
          verticalAlign: 'top',
        }}
      >
        {repo.repository}
      </td>
      <td
        style={{
          paddingInline: '1rem',
          paddingBlock: '0.75rem',
          fontSize: 'var(--text-body)',
          color: 'var(--fg-muted)',
          verticalAlign: 'top',
        }}
      >
        {repo.project_name}
      </td>
      <td
        style={{
          paddingInline: '1rem',
          paddingBlock: '0.75rem',
          fontSize: 'var(--text-body)',
          color: 'var(--fg-muted)',
          verticalAlign: 'top',
        }}
      >
        {repo.service_name}
      </td>
      <td
        style={{
          paddingInline: '1rem',
          paddingBlock: '0.75rem',
          verticalAlign: 'top',
        }}
      >
        <div className="stack" style={{ gap: '0.35rem' }}>
          {repo.tags.map((t) => (
            <TagChip key={t.tag} tag={t} />
          ))}
          {repo.tags.length === 0 ? (
            <span className="text-faint" style={{ fontSize: 'var(--text-body)' }}>—</span>
          ) : null}
        </div>
      </td>
      <td
        style={{
          paddingInline: '1rem',
          paddingBlock: '0.75rem',
          verticalAlign: 'middle',
          textAlign: 'right',
          whiteSpace: 'nowrap',
        }}
      >
        <button
          type="button"
          className="btn"
          style={{ fontSize: 'var(--text-label)', padding: '0.35rem 0.65rem', gap: '0.4rem' }}
          onClick={() => setOpen(true)}
          title="Show push commands"
        >
          <Terminal size={13} aria-hidden="true" />
          Push commands
        </button>
        <PushCommandsModal
          repository={repo.repository}
          open={open}
          onClose={() => setOpen(false)}
        />
      </td>
    </tr>
  )
}

function TagChip({
  tag,
}: {
  readonly tag: { tag: string; digest: string; size: number; updated_at: string }
}) {
  const shortDigest = tag.digest.startsWith('sha256:')
    ? `sha256:${tag.digest.slice(7, 19)}`
    : tag.digest.slice(0, 12)

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '0.5rem',
        flexWrap: 'wrap',
        fontSize: 'var(--text-body)',
      }}
    >
      <span
        style={{
          fontFamily: 'var(--font-mono)',
          fontWeight: 600,
          color: 'var(--fg)',
        }}
      >
        {tag.tag}
      </span>
      <span
        className="tnum"
        style={{
          fontFamily: 'var(--font-mono)',
          fontSize: 'var(--text-label)',
          color: 'var(--fg-faint)',
          background: 'var(--surface-2)',
          border: '1px solid var(--border)',
          borderRadius: '4px',
          padding: '1px 5px',
        }}
      >
        {shortDigest}
      </span>
      <span className="text-faint tnum" style={{ fontSize: 'var(--text-label)' }}>
        {formatBytes(tag.size)}
      </span>
      <span className="text-faint" style={{ fontSize: 'var(--text-label)' }}>
        {formatRelative(tag.updated_at, Date.now())}
      </span>
    </div>
  )
}

// Exported for tests.
export function GcButton({
  busy,
  onConfirm,
}: {
  readonly busy: boolean
  readonly onConfirm: () => void
}) {
  return (
    <ConfirmButton
      label={busy ? 'Running…' : 'Run garbage collection'}
      confirmLabel="Run GC"
      message="Sweep unreferenced blobs now? This runs synchronously and may take a moment on a large registry."
      onConfirm={onConfirm}
      busy={busy}
      className="btn"
      align="right"
    />
  )
}

function Stat({
  label,
  value,
  sub,
}: {
  label: string
  value: React.ReactNode
  sub?: string
}) {
  return (
    <div className="flex flex-col gap-1">
      <p className="kicker">{label}</p>
      <span className="t-title tnum">{value}</span>
      {sub ? (
        <span className="text-faint" style={{ fontSize: 'var(--text-label)' }}>
          {sub}
        </span>
      ) : null}
    </div>
  )
}
