import { describe, expect, it } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
import { Nodes } from './schema'

// baseUrl "" selects the fixture path, so no network is touched.
const TestLayer = ApiClientLive.pipe(
  Layer.provide(Layer.succeed(AppConfig)({ baseUrl: '', token: undefined })),
  Layer.provide(FetchHttpClient.layer),
)

const listNodes = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listNodes
})

describe('ApiClient', () => {
  it.effect('listNodes decodes the payload into Node values', () =>
    listNodes.pipe(
      Effect.provide(TestLayer),
      Effect.map((nodes) => {
        expect(nodes.length).toBe(3)
        expect(nodes[0].name).toBe('alice')
        expect(nodes[2].id).toBe(3)
      }),
    ),
  )
})

describe('Nodes schema', () => {
  it.effect('rejects a malformed payload as a typed failure', () =>
    Schema.decodeUnknownEffect(Nodes)([{ id: 'not-a-number', name: 'x' }]).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )
})
