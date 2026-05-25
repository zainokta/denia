import { describe, expect, it, vi } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
import { ApiError } from './errors'
import { clearToken, getToken, setToken, subscribe } from './auth-store'
import { LoginResult, Me } from './schema'

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
  it.effect('ApiClient has auth + service methods', () =>
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

const FIXTURE_PROJECT = {
  id: '018f1100-0000-7000-0000-000000000001',
  name: 'web',
  description: null,
  shared_env: [{ key: 'A', value: '1' }],
  default_resource_limits: null,
  created_at: '2026-05-25T00:00:00Z',
}

const emptyApi = () =>
  Effect.succeed(undefined) as Effect.Effect<never, never, never>

const mockApi = (success = true) =>
  Layer.succeed(ApiClient)({
    listNodes: emptyApi() as never,
    login: ((_u: string, _p: string) => emptyApi()) as never,
    logout: emptyApi() as never,
    me: emptyApi() as never,
    listUsers: emptyApi() as never,
    createUser: ((_u: string, _p: string) => emptyApi()) as never,
    deleteUser: ((_id: number) => emptyApi()) as never,
    listApiTokens: emptyApi() as never,
    createApiToken: ((_n: string) => emptyApi()) as never,
    deleteApiToken: ((_id: number) => emptyApi()) as never,
    listMembers: ((_pid: number) => emptyApi()) as never,
    addMember: ((_pid: number, _uid: number, _r: string) => emptyApi()) as never,
    removeMember: ((_pid: number, _uid: number) => emptyApi()) as never,
    listServices: emptyApi() as never,
    getServiceDeployments: ((_id: number) => emptyApi()) as never,
    getServiceLogs: ((_id: number) => emptyApi()) as never,
    getServiceMetrics: ((_id: number) => emptyApi()) as never,
    createDeployment: ((_input: { service_id: number }) => emptyApi()) as never,
    stopService: ((_id: number) => emptyApi()) as never,
    listProjects: Effect.succeed(
      [FIXTURE_PROJECT] as ReadonlyArray<typeof FIXTURE_PROJECT>,
    ) as never,
    getProject: ((_id: string) => Effect.succeed(FIXTURE_PROJECT)) as never,
    createProject: ((_input: never) => Effect.succeed(FIXTURE_PROJECT)) as never,
    deleteProject: ((_id: string) =>
      success
        ? Effect.void
        : Effect.fail(
            new ApiError({
              message: 'HTTP 409: {"message":"project has services"}',
              status: 409,
            }),
          )) as never,
  })

describe('ApiClient projects', () => {
  it.effect('project methods exist on ApiClient', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      expect(typeof api.listProjects).toBe('object')
      expect(typeof api.getProject).toBe('function')
      expect(typeof api.createProject).toBe('function')
      expect(typeof api.deleteProject).toBe('function')
    }).pipe(Effect.provide(mockApi())),
  )

  it.effect('listProjects decodes an array of projects', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const projects = yield* api.listProjects
      expect(projects.length).toBe(1)
      expect(projects[0].name).toBe('web')
    }).pipe(Effect.provide(mockApi())),
  )

  it.effect('getProject returns a single project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const p = yield* api.getProject('x')
      expect(p.name).toBe('web')
    }).pipe(Effect.provide(mockApi())),
  )

  it.effect('createProject returns created project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const p = yield* api.createProject({
        name: 'x',
        description: null,
        shared_env: [],
        default_resource_limits: null,
      } as never)
      expect(p.name).toBe('web')
    }).pipe(Effect.provide(mockApi())),
  )

  it.effect('deleteProject succeeds for empty project', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      yield* api.deleteProject('x')
    }).pipe(Effect.provide(mockApi())),
  )

  it.effect('deleteProject maps 409 to ApiError', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      return yield* api.deleteProject('x')
    }).pipe(
      Effect.provide(mockApi(false)),
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeInstanceOf(ApiError)
        expect((error as ApiError).message).toContain('409')
      }),
    ),
  )
})
