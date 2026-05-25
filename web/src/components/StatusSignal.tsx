interface Props {
  status: string
}

const statusClass: Record<string, string> = {
  Healthy: 'signal-ok',
  Failed: 'signal-fault',
  Building: 'signal-warn',
  Starting: 'signal-warn',
  Pending: 'signal-warn',
}

export function StatusSignal({ status }: Props) {
  const cls = statusClass[status]
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
      {cls ? <span className={`signal ${cls}`} aria-hidden="true" /> : null}
      <span>{status}</span>
    </span>
  )
}
