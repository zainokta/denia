import { Context, Effect, Layer, Schema } from 'effect'
import { HttpClient } from 'effect/unstable/http'
import { AppConfig } from './config'
import { ApiError, DecodeError } from './errors'
import { Node, Nodes } from './schema'

export class ApiClient extends Context.Service<
  ApiClient,
  {
    readonly listNodes: Effect.Effect<ReadonlyArray<Node>, ApiError | DecodeError>
  }
>()('ApiClient') {}

// Stand-in for the wire payload until the control-plane /v1/nodes endpoint is wired.
const FIXTURE: unknown = [
  { id: 1, name: 'alice' },
  { id: 2, name: 'bob' },
  { id: 3, name: 'charlie' },
]

export const ApiClientLive = Layer.effect(ApiClient)(
  Effect.gen(function* () {
    const config = yield* AppConfig
    const http = yield* HttpClient.HttpClient

    const decode = (input: unknown) =>
      Schema.decodeUnknownEffect(Nodes)(input).pipe(
        Effect.mapError(
          (error) => new DecodeError({ message: String(error) }),
        ),
      )

    const fromHttp = Effect.gen(function* () {
      const headers = config.token
        ? { authorization: `Bearer ${config.token}` }
        : {}
      const response = yield* http
        .get(`${config.baseUrl}/v1/nodes`, { headers })
        .pipe(
          Effect.mapError((error) => new ApiError({ message: String(error) })),
        )
      const body = yield* response.json.pipe(
        Effect.mapError((error) => new ApiError({ message: String(error) })),
      )
      return yield* decode(body)
    })

    const listNodes: Effect.Effect<
      ReadonlyArray<Node>,
      ApiError | DecodeError
    > = config.baseUrl === '' ? decode(FIXTURE) : fromHttp

    return { listNodes }
  }),
)
