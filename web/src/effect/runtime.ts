import { Cause, Effect, Exit, Layer, ManagedRuntime } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfigLive } from './config'

const MainLayer = ApiClientLive.pipe(
  Layer.provide(AppConfigLive),
  Layer.provide(FetchHttpClient.layer),
)

const runtime = ManagedRuntime.make(MainLayer)

// Reject with the clean typed failure (ApiError / DecodeError) rather than a
// wrapped fiber failure, so TanStack Query error handlers can read `.status`
// (e.g. to redirect on 401) and `.message` directly.
export async function runQuery<A, E>(
  effect: Effect.Effect<A, E, ApiClient>,
): Promise<A> {
  const exit = await runtime.runPromiseExit(effect)
  if (Exit.isSuccess(exit)) return exit.value
  // squash returns the representative failure (the ApiError/DecodeError for a
  // typed fail, or the defect) so callers reject with the clean error.
  throw Cause.squash(exit.cause)
}
