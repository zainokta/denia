import {
  HeadContent,
  Scripts,
  createRootRouteWithContext,
  redirect,
  useNavigate,
  useRouterState,
} from '@tanstack/react-router'
import { useEffect } from 'react'
import { TanStackRouterDevtoolsPanel } from '@tanstack/react-router-devtools'
import { TanStackDevtools } from '@tanstack/react-devtools'
import Footer from '../components/Footer'
import Header from '../components/Header'
import Sidebar from '../components/Sidebar'

import TanStackQueryDevtools from '../integrations/tanstack-query/devtools'

import appCss from '../styles.css?url'

import { useQueryClient } from '@tanstack/react-query'
import type { QueryClient } from '@tanstack/react-query'
import { captureTokenFromUrl, clearToken, getToken } from '../effect/auth-store'
import { useAuth } from '../hooks/useAuth'

// Capture a `?token=...` from the launch URL into storage before any
// `beforeLoad` auth gate runs, then strip it from the address bar.
captureTokenFromUrl()

interface MyRouterContext {
  queryClient: QueryClient
}

// Public routes that do not require an authenticated session.
const PUBLIC_ROUTES = ['/login', '/setup']

function hasAuth(): boolean {
  if (getToken()) return true
  // DEV-ONLY: a build-time token grants access. Guarded by `import.meta.env.DEV`
  // so Vite strips this branch (and the token literal) from production builds.
  if (
    typeof import.meta !== 'undefined' &&
    import.meta.env.DEV &&
    typeof import.meta.env.VITE_DENIA_TOKEN === 'string' &&
    import.meta.env.VITE_DENIA_TOKEN.length > 0
  )
    return true
  return false
}

const THEME_INIT_SCRIPT = `(function(){try{var stored=window.localStorage.getItem('theme');var theme=(stored==='light'||stored==='dark')?stored:(window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light');var root=document.documentElement;root.classList.remove('light','dark');root.classList.add(theme);root.setAttribute('data-theme',theme);root.style.colorScheme=theme;}catch(e){}})();`

export const Route = createRootRouteWithContext<MyRouterContext>()({
  beforeLoad: ({ location }) => {
    const isPublicRoute = PUBLIC_ROUTES.includes(location.pathname)
    const isLoginRoute = location.pathname === '/login'
    if (!hasAuth() && !isPublicRoute) {
      throw redirect({ to: '/login' })
    }
    if (hasAuth() && isLoginRoute) {
      throw redirect({ to: '/' })
    }
  },
  head: () => ({
    meta: [
      {
        charSet: 'utf-8',
      },
      {
        name: 'viewport',
        content: 'width=device-width, initial-scale=1',
      },
      {
        title: 'Denia',
      },
    ],
    links: [
      {
        rel: 'stylesheet',
        href: appCss,
      },
    ],
  }),
  shellComponent: RootDocument,
})

function Chrome({ children }: { children: React.ReactNode }) {
  const pathname = useRouterState({ select: (s) => s.location.pathname })

  if (pathname === '/login' || pathname === '/setup') {
    return <main id="main">{children}</main>
  }

  return (
    <div className="app-shell">
      <Header />
      <div className="app-body">
        <Sidebar />
        <div className="app-main-col">
          <main id="main">{children}</main>
          <Footer />
        </div>
      </div>
    </div>
  )
}

// Redirects bootstrap principals to/from `/setup` AFTER `me()` resolves.
// Never redirects during render; does nothing while loading or token-less.
function BootstrapGate({ children }: { children: React.ReactNode }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { token, isLoading, isBootstrap, adminInitialized } = useAuth()
  const pathname = useRouterState({ select: (s) => s.location.pathname })

  useEffect(() => {
    if (isLoading || !token) return
    if (isBootstrap && !adminInitialized && pathname !== '/setup') {
      navigate({ to: '/setup' })
    } else if (isBootstrap && adminInitialized && pathname === '/setup') {
      // Already initialized but a stale bootstrap token is in storage. Clear it
      // (and the cached `me`) so the root `beforeLoad` won't bounce /login -> /.
      clearToken()
      queryClient.removeQueries({ queryKey: ['me'] })
      navigate({ to: '/login' })
    }
  }, [
    token,
    isLoading,
    isBootstrap,
    adminInitialized,
    pathname,
    navigate,
    queryClient,
  ])

  return <>{children}</>
}

function RootDocument({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT_SCRIPT }} />
        <HeadContent />
      </head>
      <body className="font-mono antialiased [overflow-wrap:anywhere] selection:bg-[color-mix(in_oklab,var(--pink)_28%,transparent)]">
        <a href="#main" className="skip-link">
          Skip to content
        </a>
        <Chrome>
          <BootstrapGate>{children}</BootstrapGate>
        </Chrome>
        <TanStackDevtools
          config={{
            position: 'bottom-right',
          }}
          plugins={[
            {
              name: 'Tanstack Router',
              render: <TanStackRouterDevtoolsPanel />,
            },
            TanStackQueryDevtools,
          ]}
        />
        <Scripts />
      </body>
    </html>
  )
}
