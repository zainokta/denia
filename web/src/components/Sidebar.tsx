import { Link } from '@tanstack/react-router'
import { useState } from 'react'
import {
  Activity,
  FolderTree,
  LayoutDashboard,
  ListChecks,
  Network,
  PanelLeft,
  PanelLeftClose,
  Server,
} from 'lucide-react'

const SECTIONS = [
  { to: '/', label: 'Overview', exact: true, icon: LayoutDashboard },
  { to: '/services', label: 'Services', icon: Server },
  { to: '/ingress', label: 'Ingress', icon: Network },
  { to: '/observability', label: 'Observability', icon: Activity },
  { to: '/jobs', label: 'Jobs', icon: ListChecks },
  { to: '/projects', label: 'Projects', icon: FolderTree },
] as const

const STORAGE_KEY = 'sidebar-collapsed'

function getInitialCollapsed(): boolean {
  if (typeof window === 'undefined') return false
  return window.localStorage.getItem(STORAGE_KEY) === 'true'
}

export default function Sidebar() {
  const [collapsed, setCollapsed] = useState(getInitialCollapsed)

  function toggle() {
    setCollapsed((c) => {
      const next = !c
      window.localStorage.setItem(STORAGE_KEY, String(next))
      return next
    })
  }

  return (
    <aside
      className={`app-sidebar${collapsed ? ' is-collapsed' : ''}`}
      aria-label="Sections"
    >
      <button
        type="button"
        onClick={toggle}
        className="sidebar-toggle"
        aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        aria-expanded={!collapsed}
        title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
      >
        {collapsed ? (
          <PanelLeft size={16} aria-hidden="true" />
        ) : (
          <PanelLeftClose size={16} aria-hidden="true" />
        )}
      </button>
      <nav className="sidebar-nav">
        {SECTIONS.map((s) => {
          const Icon = s.icon
          return (
            <Link
              key={s.to}
              to={s.to}
              className="sidebar-link"
              aria-label={s.label}
              title={collapsed ? s.label : undefined}
              activeProps={{
                className: 'sidebar-link is-active',
                'aria-current': 'page',
              }}
              activeOptions={'exact' in s ? { exact: true } : undefined}
            >
              <Icon size={16} className="sidebar-icon" aria-hidden="true" />
              <span className="sidebar-label">{s.label}</span>
            </Link>
          )
        })}
      </nav>
    </aside>
  )
}
