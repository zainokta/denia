import { describe, expect, it, vi } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import { FetchHttpClient } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
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
