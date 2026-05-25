interface Props {
  security?: {
    userns: boolean
    mapped_uid: number | null
    no_new_privs: boolean
    caps_dropped: boolean
  }
}

export function SecurityBadge({ security }: Props) {
  if (!security) {
    return (
      <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
        posture: n/a
      </span>
    )
  }

  const hardened = security.userns && security.no_new_privs && security.caps_dropped

  if (hardened) {
    return (
      <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
        <span className="signal signal-steady" aria-hidden="true" />
        sandboxed
      </span>
    )
  }

  const gaps: string[] = []
  if (!security.userns) gaps.push('userns')
  if (!security.no_new_privs) gaps.push('no_new_privs')
  if (!security.caps_dropped) gaps.push('caps')

  return (
    <span
      className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]"
      title={`Gaps: ${gaps.join(', ')}`}
    >
      <span className="signal signal-fault" aria-hidden="true" />
      weak: {gaps.join(', ')}
    </span>
  )
}
