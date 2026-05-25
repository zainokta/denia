import type { JobRunStatus } from '#/effect/schema'

interface Props {
  status: JobRunStatus
}

const statusClass: Record<string, string> = {
  Succeeded: 'signal-ok',
  Failed: 'signal-fault',
  Running: 'signal-warn',
  Pending: 'signal-warn',
}

export function RunStatusSignal({ status }: Props) {
  const cls = statusClass[status]
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
      <span className={`signal ${cls ?? ''}`} aria-hidden="true" />
      <span>{status}</span>
    </span>
  )
}
