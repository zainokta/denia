import {
  badgeClass,
  deploymentState,
  domainState,
  runState,
  signalClass,
  type SemState,
} from '#/lib/status'

type Kind = 'deployment' | 'run' | 'domain'

function stateFor(kind: Kind, status: string): SemState {
  if (kind === 'run') return runState(status)
  if (kind === 'domain') return domainState(status)
  return deploymentState(status)
}

// Status chip: a state dot (the signal) inside a quiet labelled chip. Color
// carries meaning; the chip surface stays muted (Signal Rule).
export function StatusBadge({
  status,
  kind = 'deployment',
  label,
}: {
  readonly status: string
  readonly kind?: Kind
  readonly label?: string
}) {
  const state = stateFor(kind, status)
  return (
    <span className={badgeClass(state)}>
      <span className={signalClass(state)} aria-hidden="true" />
      {label ?? status}
    </span>
  )
}
