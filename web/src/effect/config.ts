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

const bootstrapToken = asString(
  typeof import.meta !== 'undefined'
    ? import.meta.env.VITE_DENIA_TOKEN
    : undefined,
)

export const AppConfigLive = Layer.succeed(AppConfig)({
  baseUrl:
    asString(
      typeof import.meta !== 'undefined'
        ? import.meta.env.VITE_DENIA_API_URL
        : undefined,
    ) ?? '',
  getAuthToken: () => getToken() ?? bootstrapToken,
})
