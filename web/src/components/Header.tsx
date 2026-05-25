import { Link } from '@tanstack/react-router'
import { Suspense, lazy } from 'react'
import ThemeToggle from './ThemeToggle'

const ProjectSwitcher = lazy(() =>
  import('./ProjectSwitcher').then((m) => ({ default: m.ProjectSwitcher })),
)

export default function Header() {
  return (
    <header className="sticky top-0 z-50 border-b border-[var(--border)] bg-[var(--bg)] px-4">
      <nav className="page-wrap flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
        <Link
          to="/"
          className="flex flex-shrink-0 items-center gap-2 text-sm font-semibold tracking-tight text-[var(--fg)] no-underline hover:no-underline"
        >
          <span className="signal signal-steady" aria-hidden="true" />
          denia
          <span className="kicker ml-1">control</span>
        </Link>

        <div className="order-3 flex w-full flex-wrap items-center gap-x-5 gap-y-1 sm:order-none sm:w-auto sm:flex-nowrap">
          <Link
            to="/"
            className="nav-link"
            activeProps={{ className: 'nav-link is-active' }}
            activeOptions={{ exact: true }}
          >
            Overview
          </Link>
          <Link
            to="/services"
            className="nav-link"
            activeProps={{ className: 'nav-link is-active' }}
          >
            Services
          </Link>
          <Link
            to="/ingress"
            className="nav-link"
            activeProps={{ className: 'nav-link is-active' }}
          >
            Ingress
          </Link>
          <Link
            to="/projects"
            className="nav-link"
            activeProps={{ className: 'nav-link is-active' }}
          >
            Projects
          </Link>
          <Link
            to="/about"
            className="nav-link"
            activeProps={{ className: 'nav-link is-active' }}
          >
            About
          </Link>
          <a
            href="/demo/tanstack-query"
            className="nav-link"
          >
            Live data
          </a>
          <a
            href="https://tanstack.com/start/latest/docs/framework/react/overview"
            className="nav-link"
            target="_blank"
            rel="noreferrer"
          >
            Docs
          </a>
        </div>

        <div className="ml-auto flex items-center gap-2">
          <Suspense fallback={null}>
            <ProjectSwitcher />
          </Suspense>
          <ThemeToggle />
        </div>
      </nav>
    </header>
  )
}
