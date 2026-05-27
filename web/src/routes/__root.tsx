import {
  HeadContent,
  Scripts,
  createRootRouteWithContext,
  redirect,
  useRouterState,
} from '@tanstack/react-router'
import { TanStackRouterDevtoolsPanel } from '@tanstack/react-router-devtools'
import { TanStackDevtools } from '@tanstack/react-devtools'
import Footer from '../components/Footer'
import Header from '../components/Header'
import Sidebar from '../components/Sidebar'

import TanStackQueryDevtools from '../integrations/tanstack-query/devtools'

import appCss from '../styles.css?url'

import type { QueryClient } from '@tanstack/react-query'
import { getToken } from '../effect/auth-store'

interface MyRouterContext {
  queryClient: QueryClient
}

function hasAuth(): boolean {
  if (getToken()) return true
  if (
    typeof import.meta !== 'undefined' &&
    typeof import.meta.env.VITE_DENIA_TOKEN === 'string' &&
    import.meta.env.VITE_DENIA_TOKEN.length > 0
  )
    return true
  return false
}

const THEME_INIT_SCRIPT = `(function(){try{var stored=window.localStorage.getItem('theme');var theme=(stored==='light'||stored==='dark')?stored:(window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light');var root=document.documentElement;root.classList.remove('light','dark');root.classList.add(theme);root.setAttribute('data-theme',theme);root.style.colorScheme=theme;}catch(e){}})();`

export const Route = createRootRouteWithContext<MyRouterContext>()({
  beforeLoad: ({ location }) => {
    const isLoginRoute = location.pathname === '/login'
    if (!hasAuth() && !isLoginRoute) {
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

  if (pathname === '/login') {
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
        <Chrome>{children}</Chrome>
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
