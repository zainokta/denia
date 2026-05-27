import { defineConfig } from 'vite'
import { devtools } from '@tanstack/devtools-vite'

import { tanstackStart } from '@tanstack/react-start/plugin/vite'

import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// Refuse to bake a bearer token into the public production bundle. The token
// would be recoverable by any unauthenticated client that fetches the SPA.
if (process.env.NODE_ENV === 'production' && process.env.VITE_DENIA_TOKEN) {
  throw new Error(
    'VITE_DENIA_TOKEN must not be set for production builds — it would be embedded in the public bundle. Use runtime login instead.',
  )
}

const config = defineConfig({
  resolve: { tsconfigPaths: true },
  server: {
    proxy: {
      '/v1': { target: 'http://127.0.0.1:7180', changeOrigin: true },
      '/healthz': { target: 'http://127.0.0.1:7180', changeOrigin: true },
    },
  },
  plugins: [
    devtools(),
    tailwindcss(),
    tanstackStart({ spa: { enabled: true } }),
    viteReact(),
  ],
})

export default config
