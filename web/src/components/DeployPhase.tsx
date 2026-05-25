interface Props {
  status: string
}

const STEPS = ['queued', 'acquiring', 'starting', 'live']

const phaseIdx: Record<string, number> = {
  Pending: 0,
  Building: 1,
  Starting: 2,
  Healthy: 3,
  Failed: 3,
  Stopped: 3,
}

const phaseState: Record<string, 'warn' | 'ok' | 'fault' | 'muted'> = {
  Pending: 'warn',
  Building: 'warn',
  Starting: 'warn',
  Healthy: 'ok',
  Failed: 'fault',
  Stopped: 'muted',
}

function signalClass(state: string, isMuted: boolean): string {
  if (isMuted) return 'signal opacity-30'
  return `signal signal-${state}`
}

export function DeployPhase({ status }: Props) {
  const idx = phaseIdx[status]
  const state = phaseState[status]
  if (idx === undefined) return null

  const isMuted = state === 'muted'

  return (
    <div className="flex flex-wrap items-center gap-1">
      {STEPS.map((label, i) => {
        const isActive = i === idx
        const isDone = i < idx
        const isIdle = i > idx
        const isLast = i === STEPS.length - 1

        const showDot = isActive || isDone

        return (
          <span key={i} className="inline-flex items-center gap-1">
            <span
              className={
                isMuted || isIdle
                  ? 'kicker text-[var(--fg-muted)]'
                  : 'kicker'
              }
            >
              {label}
            </span>
            {showDot ? (
              <span
                className={signalClass(isActive ? state : 'ok', isMuted)}
                aria-hidden="true"
              />
            ) : null}
            {!isLast ? (
              <span className="text-[var(--fg-muted)] mx-0.5 opacity-40">
                —
              </span>
            ) : null}
          </span>
        )
      })}
    </div>
  )
}
