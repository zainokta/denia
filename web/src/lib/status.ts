// Single source of truth mapping backend status strings to the dual-state
// semantic palette. Pink = steady (Stagecraft), violet = fault (Breakdown);
// ok/warn are the tertiary state hues; muted = inactive / terminal-neutral.

export type SemState = 'steady' | 'ok' | 'warn' | 'fault' | 'muted'

const DEPLOYMENT_STATE: Record<string, SemState> = {
  Healthy: 'ok',
  Running: 'steady',
  Building: 'warn',
  Starting: 'warn',
  Pending: 'warn',
  Failed: 'fault',
  Stopped: 'muted',
}

const RUN_STATE: Record<string, SemState> = {
  Succeeded: 'ok',
  Running: 'steady',
  Pending: 'warn',
  Failed: 'fault',
  Skipped: 'muted',
}

const DOMAIN_STATE: Record<string, SemState> = {
  verified: 'ok',
  pending: 'warn',
  failed: 'fault',
}

export function deploymentState(status: string): SemState {
  return DEPLOYMENT_STATE[status] ?? 'muted'
}

export function runState(status: string): SemState {
  return RUN_STATE[status] ?? 'muted'
}

export function domainState(status: string): SemState {
  return DOMAIN_STATE[status] ?? 'muted'
}

// `.signal-*` modifier for a state dot. `muted` intentionally has no signal
// class so the dot reads as inactive (matches existing StatusSignal contract).
export function signalClass(state: SemState): string {
  return state === 'muted' ? 'signal opacity-40' : `signal signal-${state}`
}

// `.badge-*` modifier for a status chip.
export function badgeClass(state: SemState): string {
  return state === 'muted' ? 'badge' : `badge badge-${state}`
}
