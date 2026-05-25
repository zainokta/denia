import { Effect, Layer, ManagedRuntime } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfigLive } from './config'

const MainLayer = ApiClientLive.pipe(
  Layer.provide(AppConfigLive),
  Layer.provide(FetchHttpClient.layer),
)

const runtime = ManagedRuntime.make(MainLayer)

export function runQuery<A, E>(
  effect: Effect.Effect<A, E, ApiClient>,
): Promise<A> {
  return runtime.runPromise(effect)
}
