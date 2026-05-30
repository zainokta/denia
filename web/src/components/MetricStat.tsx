import type { ReactNode } from 'react'
import { Sparkline } from './Charts'
import type { SemState } from '#/lib/status'

const STATE_VAR: Record<SemState, string> = {
  steady: 'var(--pink)',
  ok: 'var(--ok)',
  warn: 'var(--warn)',
  fault: 'var(--violet)',
  muted: 'var(--fg-muted)',
}

// A single headline metric: kicker label, big tabular value, optional unit and
// trailing sparkline. Deliberately not the SaaS hero-metric template: no card
// chrome, no decorative accent, sits inline in a panel or grid.
export function MetricStat({
  label,
  value,
  unit,
  spark,
  state = 'steady',
  sub,
}: {
  readonly label: string
  readonly value: ReactNode
  readonly unit?: string
  readonly spark?: ReadonlyArray<number>
  readonly state?: SemState
  readonly sub?: ReactNode
}) {
  return (
    <div className="flex flex-col gap-1">
      <p className="kicker">{label}</p>
      <div className="cluster" style={{ gap: '0.6rem' }}>
        <span className="t-display tnum" style={{ lineHeight: 1 }}>
          {value}
          {unit ? (
            <span
              className="text-faint"
              style={{ fontSize: '0.5em', marginLeft: '0.15em' }}
            >
              {unit}
            </span>
          ) : null}
        </span>
        {spark && spark.length > 1 ? (
          <Sparkline
            values={spark}
            color={STATE_VAR[state]}
            width={96}
            height={30}
            ariaLabel={`${label} trend`}
          />
        ) : null}
      </div>
      {sub ? <p className="text-faint" style={{ fontSize: 'var(--text-body)' }}>{sub}</p> : null}
    </div>
  )
}
