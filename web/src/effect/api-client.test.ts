import { beforeEach, describe, expect, it, vi } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import {
  FetchHttpClient,
  HttpClient,
  HttpClientRequest,
  HttpClientResponse,
} from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
import { ApiError } from './errors'
import { clearToken, getToken, setToken, subscribe } from './auth-store'
import {
  Deployment,
  DeploymentStatus,
  Deployments,
  MetricSnapshot,
  Metrics,
  Service,
  Services,
} from './schema'

const TestLayer = ApiClientLive.pipe(
  Layer.provide(
    Layer.succeed(AppConfig)({ baseUrl: '', getAuthToken: () => undefined }),
  ),
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

describe('auth-store', () => {
  it('getToken returns undefined when nothing is stored', () => {
    clearToken()
    expect(getToken()).toBeUndefined()
  })

  it('setToken stores and getToken retrieves a token', () => {
    clearToken()
    setToken('abc')
    expect(getToken()).toBe('abc')
  })

  it('setToken overwrites a previous token', () => {
    clearToken()
    setToken('abc')
    setToken('def')
    expect(getToken()).toBe('def')
  })

  it('clearToken removes a stored token', () => {
    clearToken()
    setToken('abc')
    clearToken()
    expect(getToken()).toBeUndefined()
  })

  it('subscribe notifies listeners on setToken and clearToken', () => {
    clearToken()
    const listener = vi.fn()
    const unsub = subscribe(listener)
    setToken('abc')
    expect(listener).toHaveBeenCalledTimes(1)
    clearToken()
    expect(listener).toHaveBeenCalledTimes(2)
    unsub()
    setToken('def')
    expect(listener).toHaveBeenCalledTimes(2)
  })
})

describe('AppConfig getAuthToken', () => {
  const ConfigLayer = Layer.succeed(AppConfig)({
    baseUrl: '',
    getAuthToken: () => getToken(),
  })

  it.effect('returns runtime token when set via auth-store', () =>
    Effect.gen(function* () {
      clearToken()
      setToken('runtime-token')
      const config = yield* AppConfig
      expect(config.getAuthToken()).toBe('runtime-token')
    }).pipe(Effect.provide(ConfigLayer)),
  )

  it.effect('returns undefined when no token is set', () =>
    Effect.gen(function* () {
      clearToken()
      const config = yield* AppConfig
      expect(config.getAuthToken()).toBeUndefined()
    }).pipe(Effect.provide(ConfigLayer)),
  )

  it.effect('reads the current token per call without rebuilding runtime', () =>
    Effect.gen(function* () {
      clearToken()
      const config = yield* AppConfig
      setToken('first')
      expect(config.getAuthToken()).toBe('first')
      setToken('second')
      expect(config.getAuthToken()).toBe('second')
      clearToken()
      expect(config.getAuthToken()).toBeUndefined()
    }).pipe(Effect.provide(ConfigLayer)),
  )
})

describe('ApiClient with getAuthToken', () => {
  it.effect('listNodes uses getAuthToken from config (fixture path)', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const nodes = yield* api.listNodes
      expect(nodes.length).toBe(3)
    }).pipe(
      Effect.provide(
        ApiClientLive.pipe(
          Layer.provide(
            Layer.succeed(AppConfig)({
              baseUrl: '',
              getAuthToken: () => 'test-token',
            }),
          ),
          Layer.provide(FetchHttpClient.layer),
        ),
      ),
    ),
  )
})

const FIXTURE_PROJECT = {
  id: '018f1100-0000-7000-0000-000000000001',
  name: 'web',
  description: null,
  shared_env: [{ key: 'A', value: '1' }],
  default_resource_limits: null,
  created_at: '2026-05-25T00:00:00Z',
}

const FIXTURE_PROJECTS = [
  FIXTURE_PROJECT,
  {
    id: '018f1100-0000-7000-0000-000000000002',
    name: 'api',
    description: 'backend services',
    shared_env: [],
    default_resource_limits: { cpu_millis: 1000, memory_bytes: 536870912 },
    created_at: '2026-05-25T00:00:00Z',
  },
]

function stubClient(
  handler: (
    request: HttpClientRequest.HttpClientRequest,
  ) => Effect.Effect<HttpClientResponse.HttpClientResponse>,
): Layer.Layer<HttpClient.HttpClient> {
  return Layer.succeed(HttpClient.HttpClient)(
    HttpClient.make((request) => handler(request)),
  )
}

const ProjectTestLayer = ApiClientLive.pipe(
  Layer.provide(
    Layer.succeed(AppConfig)({
      baseUrl: 'http://x',
      getAuthToken: () => undefined,
    }),
  ),
  Layer.provide(
    stubClient((_req) =>
      Effect.succeed(
        HttpClientResponse.fromWeb(
          globalThis,
          new Response(JSON.stringify(FIXTURE_PROJECTS)),
        ),
      ),
    ),
  ),
)

describe('ApiClient projects', () => {
  it.effect('listProjects fetches and decodes projects', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.listProjects
    }).pipe(
      Effect.provide(ProjectTestLayer),
      Effect.map((projects) => {
        expect(projects.length).toBe(2)
        expect(projects[0].name).toBe('web')
        expect(projects[1].name).toBe('api')
      }),
    ),
  )

  it.effect('getProject fetches a single project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.getProject('018f1100-0000-7000-0000-000000000001')
    }).pipe(
      Effect.provide(
        ApiClientLive.pipe(
          Layer.provide(
            Layer.succeed(AppConfig)({
              baseUrl: 'http://x',
              getAuthToken: () => undefined,
            }),
          ),
          Layer.provide(
            stubClient((_req) =>
              Effect.succeed(
                HttpClientResponse.fromWeb(
                  globalThis,
                  new Response(JSON.stringify(FIXTURE_PROJECT)),
                ),
              ),
            ),
          ),
        ),
      ),
      Effect.map((p) => {
        expect(p.name).toBe('web')
        expect(p.id).toBe('018f1100-0000-7000-0000-000000000001')
      }),
    ),
  )

  it.effect('createProject posts and decodes the created project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.createProject({
        name: 'new-proj',
        description: null,
        shared_env: [],
        default_resource_limits: null,
      })
    }).pipe(
      Effect.provide(
        ApiClientLive.pipe(
          Layer.provide(
            Layer.succeed(AppConfig)({
              baseUrl: 'http://x',
              getAuthToken: () => undefined,
            }),
          ),
          Layer.provide(
            stubClient((_req) =>
              Effect.succeed(
                HttpClientResponse.fromWeb(
                  globalThis,
                  new Response(
                    JSON.stringify({
                      id: '018f-new',
                      name: 'new-proj',
                      description: null,
                      shared_env: [],
                      default_resource_limits: null,
                      created_at: '2026-05-25T00:00:00Z',
                    }),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
      Effect.map((p) => {
        expect(p.name).toBe('new-proj')
      }),
    ),
  )

  it.effect('deleteProject succeeds for an empty project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.deleteProject('018f-empty')
    }).pipe(
      Effect.provide(
        ApiClientLive.pipe(
          Layer.provide(
            Layer.succeed(AppConfig)({
              baseUrl: 'http://x',
              getAuthToken: () => undefined,
            }),
          ),
          Layer.provide(
            stubClient((_req) =>
              Effect.succeed(
                HttpClientResponse.fromWeb(
                  globalThis,
                  new Response(null, { status: 204 }),
                ),
              ),
            ),
          ),
        ),
      ),
      Effect.map((result) => {
        expect(result).toBeUndefined()
      }),
    ),
  )

  it.effect('deleteProject fails with ApiError on 409', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.deleteProject('018f-used')
    }).pipe(
      Effect.provide(
        ApiClientLive.pipe(
          Layer.provide(
            Layer.succeed(AppConfig)({
              baseUrl: 'http://x',
              getAuthToken: () => undefined,
            }),
          ),
          Layer.provide(
            stubClient((_req) =>
              Effect.succeed(
                HttpClientResponse.fromWeb(
                  globalThis,
                  new Response(
                    JSON.stringify({ message: 'project has services' }),
                    { status: 409 },
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeInstanceOf(ApiError)
        expect((error as ApiError).message).toContain('409')
      }),
    ),
  )
})

const SERVICE_SAMPLE = {
  id: 1,
  project_id: 42,
  name: 'web',
  domains: ['example.com'],
  internal_port: 3000,
}

const DEPLOYMENT_SAMPLE = {
  id: 10,
  service_id: 1,
  status: 'Healthy' as const,
  created_at: '2026-05-25T00:00:00Z',
}

const METRIC_SAMPLE = {
  service_id: 1,
  cpu_percent: 0.45,
  memory_bytes: 268435456,
  recorded_at: '2026-05-25T00:00:00Z',
}

describe('Service schema', () => {
  it.effect('decodes a valid service', () =>
    Schema.decodeUnknownEffect(Services)([SERVICE_SAMPLE]).pipe(
      Effect.map((services) => {
        expect(services.length).toBe(1)
        const svc = services[0]
        expect(svc.id).toBe(1)
        expect(svc.project_id).toBe(42)
        expect(svc.name).toBe('web')
        expect(svc.domains).toEqual(['example.com'])
        expect(svc.internal_port).toBe(3000)
      }),
    ),
  )

  it.effect('rejects missing fields', () =>
    Schema.decodeUnknownEffect(Service)({ id: 1 }).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )
})

describe('Deployment schema', () => {
  it.effect('decodes a valid deployment', () =>
    Schema.decodeUnknownEffect(Deployment)(DEPLOYMENT_SAMPLE).pipe(
      Effect.map((d) => {
        expect(d.id).toBe(10)
        expect(d.service_id).toBe(1)
        expect(d.status).toBe('Healthy')
        expect(d.created_at).toBe('2026-05-25T00:00:00Z')
      }),
    ),
  )

  it.effect('rejects missing fields', () =>
    Schema.decodeUnknownEffect(Deployment)({ id: 1 }).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )

  it.effect('validates status against DeploymentStatus union', () =>
    Effect.all(
      [
        'Pending',
        'Building',
        'Starting',
        'Healthy',
        'Failed',
        'Stopped',
      ].map((status) =>
        Schema.decodeUnknownEffect(DeploymentStatus)(status),
      ),
    ).pipe(
      Effect.map((statuses) => {
        expect(statuses.length).toBe(6)
      }),
    ),
  )

  it.effect('rejects bogus deployment status', () =>
    Schema.decodeUnknownEffect(DeploymentStatus)('Bogus' as string).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )
})

describe('MetricSnapshot schema', () => {
  it.effect('decodes a valid metric snapshot', () =>
    Schema.decodeUnknownEffect(MetricSnapshot)(METRIC_SAMPLE).pipe(
      Effect.map((m) => {
        expect(m.service_id).toBe(1)
        expect(m.cpu_percent).toBe(0.45)
        expect(m.memory_bytes).toBe(268435456)
        expect(m.recorded_at).toBe('2026-05-25T00:00:00Z')
      }),
    ),
  )

  it.effect('decodes an array of metrics', () =>
    Schema.decodeUnknownEffect(Metrics)([METRIC_SAMPLE, METRIC_SAMPLE]).pipe(
      Effect.map((metrics) => {
        expect(metrics.length).toBe(2)
      }),
    ),
  )
})
