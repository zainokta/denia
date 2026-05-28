import { Link, useNavigate } from '@tanstack/react-router'
import { Suspense, lazy } from 'react'
import ThemeToggle from './ThemeToggle'
import { useAuth } from '../hooks/useAuth'

const ProjectSwitcher = lazy(() =>
  import('./ProjectSwitcher').then((m) => ({ default: m.ProjectSwitcher })),
)

function Identity() {
  const { me, isBootstrap, isSuperAdmin, logout, token } = useAuth()
  const navigate = useNavigate()
  if (!token) return null
  const label = isBootstrap
    ? 'bootstrap'
    : me?.principal.kind === 'user'
      ? me.principal.user.username
      : (me?.principal.kind ?? 'session')
  const handleLogout = async () => {
    await logout()
    navigate({ to: '/login' })
  }
  return (
    <div className="flex min-w-0 items-center gap-2 text-xs">
      <span
        className="kicker inline-block max-w-[8ch] truncate align-bottom sm:max-w-[14ch]"
        title={`${isSuperAdmin ? 'super admin' : 'user'}: ${label}`}
      >
        {label}
        {isSuperAdmin && label !== 'admin' ? ' · admin' : ''}
      </span>
      <button type="button" onClick={handleLogout} className="btn">
        Logout
      </button>
    </div>
  )
}

export default function Header() {
  return (
    <header className="app-topbar">
      <div className="topbar-inner">
        <Link
          to="/"
          className="nav-home flex flex-shrink-0 items-center gap-2 text-sm font-semibold tracking-tight text-[var(--fg)] no-underline hover:no-underline"
        >
          <span
            className="signal text-[var(--fg-muted)]"
            aria-hidden="true"
          />
          denia
          <span className="kicker ml-1 hidden sm:inline">control</span>
        </Link>

        <div className="topbar-utils">
          <Suspense fallback={null}>
            <ProjectSwitcher />
          </Suspense>
          <span className="topbar-divider" aria-hidden="true" />
          <Identity />
          <ThemeToggle />
        </div>
      </div>
    </header>
  )
}
