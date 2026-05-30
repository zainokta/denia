import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { useState } from 'react'
import { HardDrive } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { OciCacheGcRun } from '#/effect/schema'
import { useAuth } from '#/hooks/useAuth'
import { useActionToasts } from '#/components/Toast'
import { ConfirmButton } from '#/components/ConfirmButton'
import { EmptyState } from '#/components/EmptyState'
import { ErrorPanel, errorMessage } from '#/components/ErrorPanel'
import { SkeletonRows } from '#/components/Skeleton'
import { Num } from '#/components/Num'
import { ApiError } from '#/effect/errors'
import { formatBytes, formatDuration, formatRelative } from '#/lib/format'

const getOciCache = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.getOciCache
})

const runGc = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.runOciCacheGc
})

export const Route = createFileRoute('/settings/oci-cache')({
  component: OciCachePage,
})

function isNotConfigured(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404
}

function OciCachePage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const [lastRun, setLastRun] = useState<OciCacheGcRun | null>(null)

  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ['oci', 'cache'],
    queryFn: () => runQuery(getOciCache),
    enabled: auth.isSuperAdmin,
    retry: false,
  })

  const gcMut = useMutation({
    mutationFn: () => runQuery(runGc),
    onSuccess: (run) => {
      setLastRun(run)
      queryClient.setQueryData(['oci', 'cache'], {
        entries: run.entries,
        total_bytes: run.total_bytes,
        oldest_entry_age_secs: run.oldest_entry_age_secs,
        last_gc_at: run.last_gc_at,
        last_gc_deleted_bytes: run.last_gc_deleted_bytes,
        last_gc_deleted_entries: run.last_gc_deleted_entries,
      })
      toast.ok(
        `GC reclaimed ${formatBytes(run.deleted_bytes)} across ${run.deleted_entries} entr${run.deleted_entries === 1 ? 'y' : 'ies'}.`,
      )
    },
    onError: (err) => toast.err(errorMessage(err)),
  })

  if (!auth.isSuperAdmin) {
    return (
      <div className="page-narrow px-4 pb-16 pt-10">
        <Header />
        <div className="panel">
          <EmptyState
            title="Super-admin only"
            hint="The OCI layer cache is a node-wide resource managed by the host operator."
          />
        </div>
      </div>
    )
  }

  return (
    <div className="page-wrap px-4 pb-16 pt-10">
      <Header />

      {isLoading ? (
        <SkeletonRows rows={3} />
      ) : error && isNotConfigured(error) ? (
        <div className="panel">
          <EmptyState
            icon={<HardDrive size={22} />}
            title="Layer cache not configured"
            hint="This node has no OCI layer cache. Enable it in the daemon configuration to deduplicate image layers across builds."
          />
        </div>
      ) : error ? (
        <ErrorPanel message={errorMessage(error)} onRetry={() => void refetch()} />
      ) : data ? (
        <div className="stack">
          <section className="panel panel-pad">
            <div className="panel-head">
              <p className="kicker">cache status</p>
              <ConfirmButton
                label={gcMut.isPending ? 'Running…' : 'Run garbage collection'}
                confirmLabel="Run GC"
                message="Sweep unreferenced layers now? This runs synchronously and may take a moment on a large cache."
                onConfirm={() => gcMut.mutate()}
                busy={gcMut.isPending}
                className="btn"
                align="right"
              />
            </div>
            <div className="grid gap-6" style={{ gridTemplateColumns: 'repeat(auto-fit, minmax(9rem, 1fr))' }}>
              <Stat label="entries" value={<Num>{data.entries.toLocaleString('en-US')}</Num>} />
              <Stat label="total size" value={<Num>{formatBytes(data.total_bytes)}</Num>} />
              <Stat
                label="oldest entry"
                value={
                  <Num>
                    {data.oldest_entry_age_secs !== null
                      ? formatDuration(data.oldest_entry_age_secs)
                      : '—'}
                  </Num>
                }
              />
              <Stat
                label="last GC"
                value={
                  <span className="tnum">
                    {data.last_gc_at ? formatRelative(data.last_gc_at, Date.now()) : 'never'}
                  </span>
                }
                sub={
                  data.last_gc_at
                    ? `${formatBytes(data.last_gc_deleted_bytes)} · ${data.last_gc_deleted_entries} entries`
                    : undefined
                }
              />
            </div>
          </section>

          {lastRun ? (
            <section className="panel panel-pad">
              <p className="kicker" style={{ marginBottom: '0.9rem' }}>
                last sweep
              </p>
              <div className="grid gap-6" style={{ gridTemplateColumns: 'repeat(auto-fit, minmax(8rem, 1fr))' }}>
                <Stat label="deleted" value={<Num>{formatBytes(lastRun.deleted_bytes)}</Num>} sub={`${lastRun.deleted_entries} entries`} />
                <Stat label="scanned" value={<Num>{lastRun.scanned_entries.toLocaleString('en-US')}</Num>} />
                <Stat label="kept (in use)" value={<Num>{lastRun.kept_in_use_entries.toLocaleString('en-US')}</Num>} />
                <Stat label="kept (recent)" value={<Num>{lastRun.kept_recent_entries.toLocaleString('en-US')}</Num>} />
              </div>
            </section>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}

function Header() {
  return (
    <header style={{ marginBottom: '1.5rem' }}>
      <p className="kicker">settings</p>
      <h1 className="t-display">Layer cache</h1>
      <p className="text-faint" style={{ marginTop: 6, maxWidth: '60ch' }}>
        Node-wide OCI layer cache. Build layers are deduplicated here;
        garbage collection reclaims space from layers no longer referenced by any
        artifact.
      </p>
    </header>
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
      {sub ? <span className="text-faint" style={{ fontSize: 'var(--text-label)' }}>{sub}</span> : null}
    </div>
  )
}
