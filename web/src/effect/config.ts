import { Context, Layer } from 'effect'
import { getToken } from './auth-store'

export class AppConfig extends Context.Service<
  AppConfig,
  {
    readonly baseUrl: string
    readonly getAuthToken: () => string | undefined
  }
>()('AppConfig') {}

function asString(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

// VITE_DENIA_TOKEN is a DEV-ONLY convenience. Referencing it only inside the
// `import.meta.env.DEV` branch lets Vite dead-code-eliminate it from production
// builds, so the token literal is never embedded in the public SPA bundle.
// Production builds also hard-fail in vite.config.ts if the var is set.
const devBootstrapToken =
  typeof import.meta !== 'undefined' && import.meta.env.DEV
    ? asString(import.meta.env.VITE_DENIA_TOKEN)
    : undefined

export const AppConfigLive = Layer.succeed(AppConfig)({
  baseUrl:
    asString(
      typeof import.meta !== 'undefined'
        ? import.meta.env.VITE_DENIA_API_URL
        : undefined,
    ) ?? '',
  getAuthToken: () => getToken() ?? devBootstrapToken,
})
