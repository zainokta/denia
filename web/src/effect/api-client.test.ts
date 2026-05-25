import { describe, expect, it } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
import { Nodes, Project, Projects } from './schema'

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

describe('Project schema', () => {
  it.effect('decodes a project', () =>
    Schema.decodeUnknownEffect(Project)({
      id: '018f1100-0000-7000-0000-000000000001',
      name: 'web',
      description: null,
      shared_env: [['A', '1']],
      default_resource_limits: null,
      created_at: '2026-05-25T00:00:00Z',
    }).pipe(
      Effect.map((p) => {
        expect(p.name).toBe('web')
        expect(p.shared_env).toEqual([['A', '1']])
      }),
    ),
  )

  it.effect('decodes a project with resource limits', () =>
    Schema.decodeUnknownEffect(Project)({
      id: '018f1100-0000-7000-0000-000000000002',
      name: 'api',
      description: 'backend services',
      shared_env: [],
      default_resource_limits: { cpu_millis: 500, memory_bytes: 268435456 },
      created_at: '2026-05-25T00:00:00Z',
    }).pipe(
      Effect.map((p) => {
        expect(p.name).toBe('api')
        expect(p.default_resource_limits).toEqual({
          cpu_millis: 500,
          memory_bytes: 268435456,
        })
      }),
    ),
  )

  it.effect('decodes an array of projects', () =>
    Schema.decodeUnknownEffect(Projects)([
      {
        id: '018f1100-0000-7000-0000-000000000001',
        name: 'web',
        description: null,
        shared_env: [],
        default_resource_limits: null,
        created_at: '2026-05-25T00:00:00Z',
      },
      {
        id: '018f1100-0000-7000-0000-000000000002',
        name: 'api',
        description: 'backend',
        shared_env: [['DB_URL', 'pg://localhost']],
        default_resource_limits: { cpu_millis: 1000, memory_bytes: 536870912 },
        created_at: '2026-05-25T00:00:00Z',
      },
    ]).pipe(
      Effect.map((projects) => {
        expect(projects.length).toBe(2)
        expect(projects[1].name).toBe('api')
        expect(projects[1].shared_env).toEqual([['DB_URL', 'pg://localhost']])
      }),
    ),
  )
})
