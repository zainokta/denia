import { Context, Layer } from 'effect'

export class AppConfig extends Context.Service<
  AppConfig,
  {
    readonly baseUrl: string
    readonly token: string | undefined
  }
>()('AppConfig') {}

function asString(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

export const AppConfigLive = Layer.succeed(AppConfig)({
  baseUrl: asString(import.meta.env.VITE_DENIA_API_URL) ?? '',
  token: asString(import.meta.env.VITE_DENIA_TOKEN),
})
