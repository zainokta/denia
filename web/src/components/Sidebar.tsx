import { Link } from '@tanstack/react-router'
import { useState } from 'react'
import {
  Activity,
  Boxes,
  Container,
  FolderTree,
  HardDrive,
  KeyRound,
  LayoutDashboard,
  ListChecks,
  Network,
  PanelLeft,
  PanelLeftClose,
  Server,
  Terminal,
  Users,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useAuth } from '../hooks/useAuth'

interface NavItem {
  readonly to: string
  readonly label: string
  readonly icon: LucideIcon
  readonly exact?: boolean
  readonly superAdmin?: boolean
}

const SECTIONS: ReadonlyArray<NavItem> = [
  { to: '/', label: 'Overview', exact: true, icon: LayoutDashboard },
  { to: '/services', label: 'Services', icon: Server },
  { to: '/ingress', label: 'Ingress', icon: Network },
  { to: '/registries', label: 'Registries', icon: Container },
  { to: '/observability', label: 'Observability', icon: Activity },
  { to: '/jobs', label: 'Jobs', icon: ListChecks },
  { to: '/projects', label: 'Projects', icon: FolderTree },
]

const SETTINGS: ReadonlyArray<NavItem> = [
  { to: '/settings/tokens', label: 'API tokens', icon: KeyRound },
  { to: '/settings/sessions', label: 'Sessions', icon: Terminal },
  { to: '/settings/users', label: 'Users', icon: Users, superAdmin: true },
  { to: '/settings/oci-cache', label: 'Layer cache', icon: HardDrive, superAdmin: true },
  { to: '/settings/hosted-registry', label: 'Hosted registry', icon: Boxes },
]

const STORAGE_KEY = 'sidebar-collapsed'

function getInitialCollapsed(): boolean {
  if (typeof window === 'undefined') return false
  return window.localStorage.getItem(STORAGE_KEY) === 'true'
}

function NavLink({ item, collapsed }: { item: NavItem; collapsed: boolean }) {
  const Icon = item.icon
  return (
    <Link
      to={item.to}
      className="sidebar-link"
      aria-label={item.label}
      title={collapsed ? item.label : undefined}
      activeProps={{ className: 'sidebar-link is-active', 'aria-current': 'page' }}
      activeOptions={item.exact ? { exact: true } : undefined}
    >
      <Icon size={16} className="sidebar-icon" aria-hidden="true" />
      <span className="sidebar-label">{item.label}</span>
    </Link>
  )
}

export default function Sidebar() {
  const [collapsed, setCollapsed] = useState(getInitialCollapsed)
  const { isSuperAdmin } = useAuth()

  function toggle() {
    setCollapsed((c) => {
      const next = !c
      window.localStorage.setItem(STORAGE_KEY, String(next))
      return next
    })
  }

  const settings = SETTINGS.filter((s) => !s.superAdmin || isSuperAdmin)

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
        {SECTIONS.map((s) => (
          <NavLink key={s.to} item={s} collapsed={collapsed} />
        ))}
      </nav>
      <p
        className="kicker sidebar-label"
        style={{ margin: '1.25rem 0 0.4rem', paddingInline: '0.7rem' }}
      >
        Settings
      </p>
      <nav className="sidebar-nav" aria-label="Settings">
        {settings.map((s) => (
          <NavLink key={s.to} item={s} collapsed={collapsed} />
        ))}
      </nav>
    </aside>
  )
}
