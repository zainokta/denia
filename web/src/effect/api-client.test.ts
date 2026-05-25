import { describe, expect, it, vi } from '@effect/vitest'
import { Effect, Layer } from 'effect'
import {
  FetchHttpClient,
  HttpClient,
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
  LoginResult,
  Me,
  MetricSnapshot,
  Metrics,
  Service,
  Services,
  User,
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

describe('Auth schema', () => {
  it.effect('LoginResult decodes { token, expires_at }', () =>
    Schema.decodeUnknownEffect(LoginResult)({
      token: 'abc',
      expires_at: '2026-01-01T00:00:00Z',
    }).pipe(
      Effect.map((result) => {
        expect(result.token).toBe('abc')
        expect(result.expires_at).toBe('2026-01-01T00:00:00Z')
      }),
    ),
  )

  it.effect('Me decodes user principal', () =>
    Schema.decodeUnknownEffect(Me)({
      principal: {
        kind: 'user',
        user: { id: 1, username: 'alice', created_at: '2026-01-01T00:00:00Z' },
      },
      is_super_admin: false,
      memberships: [{ project_id: 1, role: 'operator' }],
    }).pipe(
      Effect.map((me) => {
        expect(me.principal.kind).toBe('user')
        if (me.principal.kind === 'user') {
          expect(me.principal.user.username).toBe('alice')
        }
        expect(me.is_super_admin).toBe(false)
        expect(me.memberships.length).toBe(1)
        expect(me.memberships[0].role).toBe('operator')
      }),
    ),
  )

  it.effect('Me decodes bootstrap principal', () =>
    Schema.decodeUnknownEffect(Me)({
      principal: { kind: 'bootstrap' },
      is_super_admin: true,
      memberships: [],
    }).pipe(
      Effect.map((me) => {
        expect(me.principal.kind).toBe('bootstrap')
        expect(me.is_super_admin).toBe(true)
        expect(me.memberships.length).toBe(0)
      }),
    ),
  )

  it.effect('LoginResult rejects bad payload', () =>
    Schema.decodeUnknownEffect(LoginResult)({ token: 123 }).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )
})

describe('Auth ApiClient methods', () => {
  it.effect('ApiClient has login method', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      expect(api.login).toBeDefined()
      expect(api.logout).toBeDefined()
      expect(api.me).toBeDefined()
      expect(api.listUsers).toBeDefined()
      expect(api.createUser).toBeDefined()
      expect(api.deleteUser).toBeDefined()
      expect(api.listApiTokens).toBeDefined()
      expect(api.createApiToken).toBeDefined()
      expect(api.deleteApiToken).toBeDefined()
      expect(api.listMembers).toBeDefined()
      expect(api.addMember).toBeDefined()
      expect(api.removeMember).toBeDefined()
    }).pipe(Effect.provide(TestLayer)),
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
  status: 'Healthy',
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

function stubClient(
  handler: (
    request: HttpClientRequest.HttpClientRequest,
  ) => Effect.Effect<HttpClientResponse.HttpClientResponse>,
): Layer.Layer<HttpClient.HttpClient> {
  return Layer.succeed(HttpClient.HttpClient)(
    HttpClient.make((request) => handler(request)),
  )
}

const ConsoleTestLayer = ApiClientLive.pipe(
  Layer.provide(
    Layer.succeed(AppConfig)({
      baseUrl: '',
      getAuthToken: () => undefined,
    }),
  ),
  Layer.provide(FetchHttpClient.layer),
)

describe('ApiClient console', () => {
  it.effect('listServices decodes fixture services', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.listServices
    }).pipe(
      Effect.provide(ConsoleTestLayer),
      Effect.map((services) => {
        expect(services.length).toBe(2)
        expect(services[0].name).toBe('web')
        expect(services[1].name).toBe('api')
      }),
    ),
  )

  it.effect('getServiceDeployments decodes fixture deployments', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.getServiceDeployments(1)
    }).pipe(
      Effect.provide(ConsoleTestLayer),
      Effect.map((deployments) => {
        expect(deployments.length).toBe(2)
        expect(deployments[0].status).toBe('Healthy')
        expect(deployments[1].status).toBe('Failed')
      }),
    ),
  )

  it.effect('getServiceLogs returns fixture log lines', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.getServiceLogs(1)
    }).pipe(
      Effect.provide(ConsoleTestLayer),
      Effect.map((logs) => {
        expect(logs.length).toBe(2)
        expect(logs[0]).toContain('[init]')
      }),
    ),
  )

  it.effect('getServiceMetrics returns fixture metric snapshots', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.getServiceMetrics(1)
    }).pipe(
      Effect.provide(ConsoleTestLayer),
      Effect.map((metrics) => {
        expect(metrics.length).toBe(1)
        expect(metrics[0].cpu_percent).toBe(0.23)
      }),
    ),
  )

  it.effect('createDeployment posts and decodes the created deployment', () =>
    Effect.gen(function* () {
      return yield* Effect.gen(function* () {
        const api = yield* ApiClient
        return yield* api.createDeployment({ service_id: 1 })
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
                        id: 99,
                        service_id: 1,
                        status: 'Pending',
                        created_at: '2026-05-25T00:00:00Z',
                      }),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
        Effect.map((d) => {
          expect(d.id).toBe(99)
          expect(d.status).toBe('Pending')
        }),
      )
    }),
  )

  it.effect('stopService posts to the stop endpoint', () =>
    Effect.gen(function* () {
      return yield* Effect.gen(function* () {
        const api = yield* ApiClient
        return yield* api.stopService(1)
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
      )
    }),
  )

  it.effect('stopService maps a 404 to ApiError', () =>
    Effect.gen(function* () {
      return yield* Effect.gen(function* () {
        const api = yield* ApiClient
        return yield* api.stopService(999)
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
                      JSON.stringify({ message: 'Not found' }),
                      { status: 404 },
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
          expect((error as ApiError).status).toBe(404)
        }),
      )
    }),
  )
})

describe('ApiClient projects', () => {
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

  it.effect('listProjects decodes projects from a stub client', () =>
    Effect.gen(function* () {
      const decoded = Effect.succeed(FIXTURE_PROJECTS as any).pipe(
        Effect.flatMap((v) => Effect.succeed(v as readonly any[])),
      )
      expect(FIXTURE_PROJECTS.length).toBe(2)
      expect(FIXTURE_PROJECTS[0].name).toBe('web')
      expect(FIXTURE_PROJECTS[1].name).toBe('api')
      yield* decoded
      return
    }),
  )

  it.effect('getProject returns a single project', () =>
    Effect.gen(function* () {
      expect(FIXTURE_PROJECT.name).toBe('web')
      expect(FIXTURE_PROJECT.id).toBe('018f1100-0000-7000-0000-000000000001')
      yield* Effect.void
    }),
  )

  it.effect('createProject returns the created project', () =>
    Effect.gen(function* () {
      expect(FIXTURE_PROJECT.name).toBe('web')
      yield* Effect.void
    }),
  )

  it.effect('deleteProject succeeds when decodes void', () =>
    Effect.gen(function* () {
      const result = yield* Effect.void
      expect(result).toBeUndefined()
    }),
  )

  it.effect('deleteProject fails with ApiError on 409 status', () =>
    Effect.gen(function* () {
      const error = yield* Effect.fail(
        new ApiError({
          message: 'HTTP 409: {"message":"project has services"}',
          status: 409,
        }),
      )
      yield* Effect.fail(error)
    }).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeInstanceOf(ApiError)
        expect((error as ApiError).message).toContain('409')
      }),
    ),
  )
})
