import { describe, expect, it, vi } from '@effect/vitest'
import { Effect, Layer, Schema } from 'effect'
import { FetchHttpClient, HttpClient, HttpClientResponse } from 'effect/unstable/http'
import { ApiClient, ApiClientLive } from './api-client'
import { AppConfig } from './config'
import { ApiError } from './errors'
import { clearToken, getToken, setToken, subscribe } from './auth-store'
import { ArtifactRef, Deployment, Job, JobRun, JobRunStatus, LoginResult, Me, RouteView, RouteViews, SecurityPosture, Service } from './schema'

const TestLayer = ApiClientLive.pipe(
  Layer.provide(
    Layer.succeed(AppConfig)({ baseUrl: '', getAuthToken: () => undefined }),
  ),
  Layer.provide(FetchHttpClient.layer),
)

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
        user: {
          id: '019e68f7-82c7-76d0-bebc-7e7a679fe485',
          username: 'alice',
          created_at: '2026-01-01T00:00:00Z',
        },
      },
      is_super_admin: false,
      admin_initialized: true,
      memberships: [{ project_id: 'p1', role: 'operator' }],
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
      admin_initialized: false,
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

describe('RouteView schema', () => {
  it.effect('decodes a RouteView', () =>
    Schema.decodeUnknownEffect(RouteView)({
      service_name: 'web',
      domains: ['example.com', 'www.example.com'],
      tls: true,
    }).pipe(
      Effect.map((rv) => {
        expect(rv.service_name).toBe('web')
        expect(rv.domains).toEqual(['example.com', 'www.example.com'])
        expect(rv.tls).toBe(true)
      }),
    ),
  )

  it.effect('decodes an array of RouteViews', () =>
    Schema.decodeUnknownEffect(RouteViews)([
      {
        service_name: 'web',
        domains: ['example.com'],
        tls: true,
      },
      {
        service_name: 'api',
        domains: ['api.example.com'],
        tls: false,
      },
    ]).pipe(
      Effect.map((routes) => {
        expect(routes.length).toBe(2)
        expect(routes[0].service_name).toBe('web')
        expect(routes[0].tls).toBe(true)
        expect(routes[1].service_name).toBe('api')
        expect(routes[1].tls).toBe(false)
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
    listUserDirectory: emptyApi() as never,
    createUser: ((_u: string, _p: string) => emptyApi()) as never,
    bootstrap: ((_u: string, _p: string) => emptyApi()) as never,
    deleteUser: ((_id: number) => emptyApi()) as never,
    listCredentials: emptyApi() as never,
    putCredential: ((_input: { name: string; kind: string; secret_ref: string }) =>
      emptyApi()) as never,
    listSessions: emptyApi() as never,
    revokeAllSessions: emptyApi() as never,
    listApiTokens: emptyApi() as never,
    createApiToken: ((_n: string) => emptyApi()) as never,
    deleteApiToken: ((_id: number) => emptyApi()) as never,
    listMembers: ((_pid: string) => emptyApi()) as never,
    addMember: ((_pid: string, _uid: string, _r: string) => emptyApi()) as never,
    removeMember: ((_pid: string, _uid: string) => emptyApi()) as never,
    listServices: emptyApi() as never,
    getService: ((_id: string) => emptyApi()) as never,
    deleteService: ((_id: string) => Effect.void) as never,
    getServiceDeployments: ((_id: number) => emptyApi()) as never,
    getDeployment: ((_id: string) => emptyApi()) as never,
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
    putService: ((_svc: never) => emptyApi()) as never,
    listRoutes: emptyApi() as never,
    listJobs: ((_pid: string) =>
      Effect.succeed([FIXTURE_JOB] as ReadonlyArray<typeof FIXTURE_JOB>)) as never,
    getJob: ((_id: string) => Effect.succeed(FIXTURE_JOB)) as never,
    createJob: ((_input: never) => Effect.succeed(FIXTURE_JOB)) as never,
    deleteJob: ((_id: string) => Effect.void) as never,
    runJob: ((_id: string) => Effect.succeed(FIXTURE_JOB_RUN)) as never,
    listJobRuns: ((_id: string) =>
      Effect.succeed([FIXTURE_JOB_RUN] as ReadonlyArray<typeof FIXTURE_JOB_RUN>)) as never,
    getNodeMetrics: emptyApi() as never,
    listWorkloads: emptyApi() as never,
    listServiceRequests: ((_id: string) => emptyApi()) as never,
    listDomains: ((_id: number) => emptyApi()) as never,
    addDomain: ((_id: number, _hostname: string) => emptyApi()) as never,
    verifyDomain: ((_id: number, _did: string) => emptyApi()) as never,
    deleteDomain: ((_id: number, _did: string) => Effect.void) as never,
    listRegistries: ((_pid: string) => emptyApi()) as never,
    getRegistry: ((_pid: string, _rid: string) => emptyApi()) as never,
    createRegistry: ((_pid: string, _input: never) => emptyApi()) as never,
    updateRegistry: ((_pid: string, _rid: string, _input: never) =>
      emptyApi()) as never,
    deleteRegistry: ((_pid: string, _rid: string) => Effect.void) as never,
    getOciCache: emptyApi() as never,
    runOciCacheGc: emptyApi() as never,
    getHostedRegistryStatus: emptyApi() as never,
    listHostedRepositories: emptyApi() as never,
    runHostedRegistryGc: emptyApi() as never,
  })

const FIXTURE_JOB = {
  id: '018f1100-0000-7000-0000-000000000010',
  project_id: '018f1100-0000-7000-0000-000000000001',
  name: 'daily-backup',
  source: { type: 'external_image', image: 'alpine:latest', credential: null, registry_id: null, image_ref: null },
  command: ['/bin/sh', '-c', 'echo hello'],
  env: [['KEY', 'val']],
  schedule: null,
  max_retries: 0,
  next_run_at: null,
  last_enqueued_at: null,
  created_at: '2026-05-25T00:00:00Z',
}

const FIXTURE_JOB_RUN = {
  id: '018f1100-0000-7000-0000-000000000020',
  job_id: '018f1100-0000-7000-0000-000000000010',
  status: 'Succeeded',
  attempt: 1,
  exit_code: 0,
  started_at: '2026-05-25T00:00:01Z',
  finished_at: '2026-05-25T00:00:05Z',
  created_at: '2026-05-25T00:00:00Z',
}

describe('Job schemas', () => {
  it.effect('decodes a Job with null schedule', () =>
    Schema.decodeUnknownEffect(Job)(FIXTURE_JOB).pipe(
      Effect.map((job) => {
        expect(job.id).toBe('018f1100-0000-7000-0000-000000000010')
        expect(job.name).toBe('daily-backup')
        expect(job.schedule).toBeNull()
        expect(job.next_run_at).toBeNull()
        expect(job.max_retries).toBe(0)
        expect(job.env[0]).toEqual(['KEY', 'val'])
      }),
    ),
  )

  it.effect('decodes a Job with cron schedule', () =>
    Schema.decodeUnknownEffect(Job)({
      ...FIXTURE_JOB,
      name: 'cron-job',
      schedule: '*/5 * * * *',
      next_run_at: '2026-05-25T00:05:00Z',
    }).pipe(
      Effect.map((job) => {
        expect(job.schedule).toBe('*/5 * * * *')
        expect(job.next_run_at).toBe('2026-05-25T00:05:00Z')
      }),
    ),
  )

  it.effect('decodes a JobRun', () =>
    Schema.decodeUnknownEffect(JobRun)(FIXTURE_JOB_RUN).pipe(
      Effect.map((run) => {
        expect(run.status).toBe('Succeeded')
        expect(run.attempt).toBe(1)
        expect(run.exit_code).toBe(0)
        expect(run.started_at).toBe('2026-05-25T00:00:01Z')
        expect(run.finished_at).toBe('2026-05-25T00:00:05Z')
      }),
    ),
  )

  it.effect('decodes a JobRun with null exit_code and finished_at', () =>
    Schema.decodeUnknownEffect(JobRun)({
      ...FIXTURE_JOB_RUN,
      status: 'Running',
      exit_code: null,
      started_at: '2026-05-25T00:00:01Z',
      finished_at: null,
    }).pipe(
      Effect.map((run) => {
        expect(run.status).toBe('Running')
        expect(run.exit_code).toBeNull()
        expect(run.finished_at).toBeNull()
      }),
    ),
  )

  it.effect('JobRunStatus literal type checks', () =>
    Schema.decodeUnknownEffect(JobRunStatus)('Pending').pipe(
      Effect.map((s) => {
        expect(s).toBe('Pending')
      }),
    ),
  )
})

describe('ApiClient jobs', () => {
  const jobsMock = Layer.succeed(ApiClient)({
    listNodes: emptyApi() as never,
    login: ((_u: string, _p: string) => emptyApi()) as never,
    logout: emptyApi() as never,
    me: emptyApi() as never,
    listUsers: emptyApi() as never,
    listUserDirectory: emptyApi() as never,
    createUser: ((_u: string, _p: string) => emptyApi()) as never,
    bootstrap: ((_u: string, _p: string) => emptyApi()) as never,
    deleteUser: ((_id: number) => emptyApi()) as never,
    listCredentials: emptyApi() as never,
    putCredential: ((_input: { name: string; kind: string; secret_ref: string }) =>
      emptyApi()) as never,
    listSessions: emptyApi() as never,
    revokeAllSessions: emptyApi() as never,
    listApiTokens: emptyApi() as never,
    createApiToken: ((_n: string) => emptyApi()) as never,
    deleteApiToken: ((_id: number) => emptyApi()) as never,
    listMembers: ((_pid: string) => emptyApi()) as never,
    addMember: ((_pid: string, _uid: string, _r: string) => emptyApi()) as never,
    removeMember: ((_pid: string, _uid: string) => emptyApi()) as never,
    listServices: emptyApi() as never,
    getService: ((_id: string) => emptyApi()) as never,
    deleteService: ((_id: string) => Effect.void) as never,
    getServiceDeployments: ((_id: number) => emptyApi()) as never,
    getDeployment: ((_id: string) => emptyApi()) as never,
    getServiceLogs: ((_id: number) => emptyApi()) as never,
    getServiceMetrics: ((_id: number) => emptyApi()) as never,
    createDeployment: ((_input: { service_id: number }) => emptyApi()) as never,
    stopService: ((_id: number) => emptyApi()) as never,
    listProjects: emptyApi() as never,
    getProject: ((_id: string) => emptyApi()) as never,
    createProject: ((_input: never) => emptyApi()) as never,
    deleteProject: ((_id: string) => emptyApi()) as never,
    putService: ((_svc: unknown) => emptyApi()) as never,
    listRoutes: emptyApi() as never,
    listJobs: ((_pid: string) =>
      Effect.succeed([FIXTURE_JOB] as ReadonlyArray<typeof FIXTURE_JOB>)) as never,
    getJob: ((_id: string) => Effect.succeed(FIXTURE_JOB)) as never,
    createJob: ((_input: never) => Effect.succeed(FIXTURE_JOB)) as never,
    deleteJob: ((_id: string) => Effect.void) as never,
    runJob: ((_id: string) => Effect.succeed(FIXTURE_JOB_RUN)) as never,
    listJobRuns: ((_id: string) =>
      Effect.succeed([FIXTURE_JOB_RUN] as ReadonlyArray<typeof FIXTURE_JOB_RUN>)) as never,
    getNodeMetrics: emptyApi() as never,
    listWorkloads: emptyApi() as never,
    listServiceRequests: ((_id: string) => emptyApi()) as never,
    listDomains: ((_id: number) => emptyApi()) as never,
    addDomain: ((_id: number, _hostname: string) => emptyApi()) as never,
    verifyDomain: ((_id: number, _did: string) => emptyApi()) as never,
    deleteDomain: ((_id: number, _did: string) => Effect.void) as never,
    listRegistries: ((_pid: string) => emptyApi()) as never,
    getRegistry: ((_pid: string, _rid: string) => emptyApi()) as never,
    createRegistry: ((_pid: string, _input: never) => emptyApi()) as never,
    updateRegistry: ((_pid: string, _rid: string, _input: never) =>
      emptyApi()) as never,
    deleteRegistry: ((_pid: string, _rid: string) => Effect.void) as never,
    getOciCache: emptyApi() as never,
    runOciCacheGc: emptyApi() as never,
    getHostedRegistryStatus: emptyApi() as never,
    listHostedRepositories: emptyApi() as never,
    runHostedRegistryGc: emptyApi() as never,
  })

  it.effect('jobs methods exist on ApiClient', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      expect(typeof api.listJobs).toBe('function')
      expect(typeof api.getJob).toBe('function')
      expect(typeof api.createJob).toBe('function')
      expect(typeof api.deleteJob).toBe('function')
      expect(typeof api.runJob).toBe('function')
      expect(typeof api.listJobRuns).toBe('function')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('listJobs decodes an array of jobs', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const jobs = yield* api.listJobs('p1')
      expect(jobs.length).toBe(1)
      expect(jobs[0].name).toBe('daily-backup')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('getJob returns a single job', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const job = yield* api.getJob('j1')
      expect(job.name).toBe('daily-backup')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('createJob returns created job', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const job = yield* api.createJob({ name: 'test' } as never)
      expect(job.name).toBe('daily-backup')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('deleteJob succeeds', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      yield* api.deleteJob('j1')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('runJob returns a JobRun', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const run = yield* api.runJob('j1')
      expect(run.status).toBe('Succeeded')
    }).pipe(Effect.provide(jobsMock)),
  )

  it.effect('listJobRuns decodes an array of runs', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const runs = yield* api.listJobRuns('j1')
      expect(runs.length).toBe(1)
      expect(runs[0].attempt).toBe(1)
    }).pipe(Effect.provide(jobsMock)),
  )
})

describe('ArtifactRef schema', () => {
  const FIXTURE_DEPLOY_WITH_ARTIFACT = {
    id: '0190b8a0-0000-7000-8000-000000000001',
    service_id: '0190b8a0-0000-7000-8000-0000000000aa',
    status: 'Healthy',
    created_at: '2026-05-25T00:00:00Z',
    artifact: { digest: 'sha256:abc123', kind: 'OciImage' as const },
  }

  const FIXTURE_DEPLOY_WITHOUT_ARTIFACT = {
    id: '0190b8a0-0000-7000-8000-000000000002',
    service_id: '0190b8a0-0000-7000-8000-0000000000aa',
    status: 'Building',
    created_at: '2026-05-25T01:00:00Z',
  }

  it.effect('decodes Deployment with artifact ref', () =>
    Schema.decodeUnknownEffect(Deployment)(FIXTURE_DEPLOY_WITH_ARTIFACT).pipe(
      Effect.map((d) => {
        expect(d.id).toBe(FIXTURE_DEPLOY_WITH_ARTIFACT.id)
        expect(d.artifact).toBeDefined()
        expect(d.artifact!.digest).toBe('sha256:abc123')
        expect(d.artifact!.kind).toBe('OciImage')
      }),
    ),
  )

  it.effect('decodes Deployment without artifact ref', () =>
    Schema.decodeUnknownEffect(Deployment)(FIXTURE_DEPLOY_WITHOUT_ARTIFACT).pipe(
      Effect.map((d) => {
        expect(d.id).toBe(FIXTURE_DEPLOY_WITHOUT_ARTIFACT.id)
        expect(d.status).toBe('Building')
        expect(d.artifact).toBeUndefined()
      }),
    ),
  )

  it.effect('ArtifactRef rejects unknown kind', () =>
    Schema.decodeUnknownEffect(ArtifactRef)({
      digest: 'sha256:xyz',
      kind: 'UnknownKind',
    }).pipe(
      Effect.flip,
      Effect.map((error) => {
        expect(error).toBeDefined()
      }),
    ),
  )
})

describe('Service schema', () => {
  const FIXTURE_SVC_WITH_TLS = {
    id: '018f1100-0000-7000-0000-000000000031',
    project_id: '018f1100-0000-7000-0000-000000000001',
    name: 'web',
    domains: ['example.com'],
    source: {
      type: 'external_image',
      image: 'nginx:latest',
      credential: null,
      registry_id: null,
      image_ref: null,
    },
    internal_port: 3000,
    health_check: { path: '/healthz', timeout_seconds: 5 },
    env: [['KEY', 'val']],
    tls_enabled: true,
  }

  const FIXTURE_SVC_DEFAULT_TLS = {
    id: '018f1100-0000-7000-0000-000000000032',
    project_id: '018f1100-0000-7000-0000-000000000001',
    name: 'api',
    domains: ['api.example.com'],
    source: {
      type: 'external_image',
      image: 'alpine:latest',
      credential: null,
      registry_id: null,
      image_ref: null,
    },
    internal_port: 8080,
    health_check: { path: '/', timeout_seconds: 2 },
    env: [],
  }

  it.effect('decodes Service with tls_enabled true', () =>
    Schema.decodeUnknownEffect(Service)(FIXTURE_SVC_WITH_TLS).pipe(
      Effect.map((svc) => {
        expect(svc.id).toBe('018f1100-0000-7000-0000-000000000031')
        expect(svc.tls_enabled).toBe(true)
        expect(svc.source.type).toBe('external_image')
        expect(svc.health_check.path).toBe('/healthz')
        expect(svc.env[0]).toEqual(['KEY', 'val'])
      }),
    ),
  )

  it.effect('defaults tls_enabled to false when omitted', () =>
    Schema.decodeUnknownEffect(Service)(FIXTURE_SVC_DEFAULT_TLS).pipe(
      Effect.map((svc) => {
        expect(svc.id).toBe('018f1100-0000-7000-0000-000000000032')
        expect(svc.name).toBe('api')
        expect(svc.tls_enabled).toBe(false)
      }),
    ),
  )

  it.effect('SecurityPosture decodes with null mapped_uid', () =>
    Schema.decodeUnknownEffect(SecurityPosture)({
      userns: true,
      mapped_uid: null,
      no_new_privs: false,
      caps_dropped: true,
    }).pipe(
      Effect.map((p) => {
        expect(p.userns).toBe(true)
        expect(p.mapped_uid).toBeNull()
        expect(p.no_new_privs).toBe(false)
        expect(p.caps_dropped).toBe(true)
      }),
    ),
  )
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

const FIXTURE_ROUTES = [
  {
    service_name: 'web',
    domains: ['example.com'],
    tls: true,
  },
  {
    service_name: 'api',
    domains: ['api.example.com'],
    tls: false,
  },
]

const mockIngressApi = () =>
  Layer.succeed(ApiClient)({
    listNodes: emptyApi() as never,
    login: ((_u: string, _p: string) => emptyApi()) as never,
    logout: emptyApi() as never,
    me: emptyApi() as never,
    listUsers: emptyApi() as never,
    listUserDirectory: emptyApi() as never,
    createUser: ((_u: string, _p: string) => emptyApi()) as never,
    bootstrap: ((_u: string, _p: string) => emptyApi()) as never,
    deleteUser: ((_id: number) => emptyApi()) as never,
    listCredentials: emptyApi() as never,
    putCredential: ((_input: { name: string; kind: string; secret_ref: string }) =>
      emptyApi()) as never,
    listSessions: emptyApi() as never,
    revokeAllSessions: emptyApi() as never,
    listApiTokens: emptyApi() as never,
    createApiToken: ((_n: string) => emptyApi()) as never,
    deleteApiToken: ((_id: number) => emptyApi()) as never,
    listMembers: ((_pid: string) => emptyApi()) as never,
    addMember: ((_pid: string, _uid: string, _r: string) => emptyApi()) as never,
    removeMember: ((_pid: string, _uid: string) => emptyApi()) as never,
    listServices: emptyApi() as never,
    getService: ((_id: string) => emptyApi()) as never,
    deleteService: ((_id: string) => Effect.void) as never,
    getServiceDeployments: ((_id: number) => emptyApi()) as never,
    getDeployment: ((_id: string) => emptyApi()) as never,
    getServiceLogs: ((_id: number) => emptyApi()) as never,
    getServiceMetrics: ((_id: number) => emptyApi()) as never,
    createDeployment: ((_input: { service_id: number }) => emptyApi()) as never,
    stopService: ((_id: number) => emptyApi()) as never,
    listProjects: emptyApi() as never,
    getProject: ((_id: string) => emptyApi()) as never,
    createProject: ((_input: never) => emptyApi()) as never,
    deleteProject: ((_id: string) => emptyApi()) as never,
    putService: ((_svc: never) => emptyApi()) as never,
    listRoutes: Effect.succeed(FIXTURE_ROUTES as ReadonlyArray<RouteView>) as never,
    listJobs: ((_pid: string) =>
      Effect.succeed([FIXTURE_JOB] as ReadonlyArray<typeof FIXTURE_JOB>)) as never,
    getJob: ((_id: string) => Effect.succeed(FIXTURE_JOB)) as never,
    createJob: ((_input: never) => Effect.succeed(FIXTURE_JOB)) as never,
    deleteJob: ((_id: string) => Effect.void) as never,
    runJob: ((_id: string) => Effect.succeed(FIXTURE_JOB_RUN)) as never,
    listJobRuns: ((_id: string) =>
      Effect.succeed([FIXTURE_JOB_RUN] as ReadonlyArray<typeof FIXTURE_JOB_RUN>)) as never,
    getNodeMetrics: emptyApi() as never,
    listWorkloads: emptyApi() as never,
    listServiceRequests: ((_id: string) => emptyApi()) as never,
    listDomains: ((_id: number) => emptyApi()) as never,
    addDomain: ((_id: number, _hostname: string) => emptyApi()) as never,
    verifyDomain: ((_id: number, _did: string) => emptyApi()) as never,
    deleteDomain: ((_id: number, _did: string) => Effect.void) as never,
    listRegistries: ((_pid: string) => emptyApi()) as never,
    getRegistry: ((_pid: string, _rid: string) => emptyApi()) as never,
    createRegistry: ((_pid: string, _input: never) => emptyApi()) as never,
    updateRegistry: ((_pid: string, _rid: string, _input: never) =>
      emptyApi()) as never,
    deleteRegistry: ((_pid: string, _rid: string) => Effect.void) as never,
    getOciCache: emptyApi() as never,
    runOciCacheGc: emptyApi() as never,
    getHostedRegistryStatus: emptyApi() as never,
    listHostedRepositories: emptyApi() as never,
    runHostedRegistryGc: emptyApi() as never,
  })

describe('Ingress ApiClient methods', () => {
  it.effect('listRoutes decodes an array of RouteView', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const routes = yield* api.listRoutes
      expect(routes.length).toBe(2)
      expect(routes[0].service_name).toBe('web')
      expect(routes[0].tls).toBe(true)
      expect(routes[1].service_name).toBe('api')
      expect(routes[1].tls).toBe(false)
    }).pipe(Effect.provide(mockIngressApi())),
  )
})

describe('ApiClient bootstrap', () => {
  const BOOTSTRAP_USER = {
    id: '019e68f7-82c7-76d0-bebc-7e7a679fe485',
    username: 'root',
    created_at: '2026-05-27T00:00:00Z',
  }

  // Stub HttpClient that answers POST /v1/bootstrap with a 201 + User JSON.
  const StubHttp = Layer.succeed(HttpClient.HttpClient)(
    HttpClient.make((request) =>
      Effect.succeed(
        HttpClientResponse.fromWeb(
          request,
          new Response(JSON.stringify(BOOTSTRAP_USER), {
            status: 201,
            headers: { 'content-type': 'application/json' },
          }),
        ),
      ),
    ),
  )

  const BootstrapLayer = ApiClientLive.pipe(
    Layer.provide(
      Layer.succeed(AppConfig)({
        baseUrl: 'http://denia.test',
        getAuthToken: () => undefined,
      }),
    ),
    Layer.provide(StubHttp),
  )

  it.effect('bootstrap POSTs and decodes the created User', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const user = yield* api.bootstrap('root', 'supersecret')
      expect(user.username).toBe('root')
    }).pipe(Effect.provide(BootstrapLayer)),
  )
})

describe('putService', () => {
  const FIXTURE_SVC = {
    id: '018f1100-0000-7000-0000-000000000041',
    project_id: '018f1100-0000-7000-0000-000000000001',
    name: 'web',
    domains: ['example.com'],
    source: {
      type: 'external_image',
      image: 'nginx:latest',
      credential: null,
      registry_id: null,
      image_ref: null,
    },
    internal_port: 3000,
    health_check: { path: '/healthz', timeout_seconds: 5 },
    env: [],
    tls_enabled: true,
  }

  it.effect('putService updates a service', () =>
    Effect.gen(function* () {
      const api = yield* ApiClient
      const svc = yield* api.putService(FIXTURE_SVC as never)
      expect(svc.name).toBe('web')
      expect(svc.tls_enabled).toBe(true)
    }).pipe(
      Effect.provide(
        Layer.succeed(ApiClient)({
          listNodes: emptyApi() as never,
          login: ((_u: string, _p: string) => emptyApi()) as never,
          logout: emptyApi() as never,
          me: emptyApi() as never,
          listUsers: emptyApi() as never,
          listUserDirectory: emptyApi() as never,
          createUser: ((_u: string, _p: string) => emptyApi()) as never,
          bootstrap: ((_u: string, _p: string) => emptyApi()) as never,
          deleteUser: ((_id: number) => emptyApi()) as never,
          listCredentials: emptyApi() as never,
          putCredential: ((_input: {
            name: string
            kind: string
            secret_ref: string
          }) => emptyApi()) as never,
          listSessions: emptyApi() as never,
          revokeAllSessions: emptyApi() as never,
          listApiTokens: emptyApi() as never,
          createApiToken: ((_n: string) => emptyApi()) as never,
          deleteApiToken: ((_id: number) => emptyApi()) as never,
          listMembers: ((_pid: string) => emptyApi()) as never,
          addMember: ((_pid: string, _uid: string, _r: string) => emptyApi()) as never,
          removeMember: ((_pid: string, _uid: string) => emptyApi()) as never,
          listServices: emptyApi() as never,
          getService: ((_id: string) => emptyApi()) as never,
          deleteService: ((_id: string) => Effect.void) as never,
          getServiceDeployments: ((_id: number) => emptyApi()) as never,
          getDeployment: ((_id: string) => emptyApi()) as never,
          getServiceLogs: ((_id: number) => emptyApi()) as never,
          getServiceMetrics: ((_id: number) => emptyApi()) as never,
          createDeployment: ((_input: { service_id: number }) => emptyApi()) as never,
          stopService: ((_id: number) => emptyApi()) as never,
          listProjects: emptyApi() as never,
          getProject: ((_id: string) => emptyApi()) as never,
          createProject: ((_input: never) => emptyApi()) as never,
          deleteProject: ((_id: string) => emptyApi()) as never,
          putService: ((_svc: unknown) => Effect.succeed(FIXTURE_SVC)) as never,
          listRoutes: emptyApi() as never,
          listJobs: ((_pid: string) =>
            Effect.succeed([FIXTURE_JOB] as ReadonlyArray<typeof FIXTURE_JOB>)) as never,
          getJob: ((_id: string) => Effect.succeed(FIXTURE_JOB)) as never,
          createJob: ((_input: never) => Effect.succeed(FIXTURE_JOB)) as never,
          deleteJob: ((_id: string) => Effect.void) as never,
          runJob: ((_id: string) => Effect.succeed(FIXTURE_JOB_RUN)) as never,
          listJobRuns: ((_id: string) =>
            Effect.succeed([FIXTURE_JOB_RUN] as ReadonlyArray<typeof FIXTURE_JOB_RUN>)) as never,
          getNodeMetrics: emptyApi() as never,
          listWorkloads: emptyApi() as never,
          listServiceRequests: ((_id: string) => emptyApi()) as never,
          listDomains: ((_id: number) => emptyApi()) as never,
          addDomain: ((_id: number, _hostname: string) => emptyApi()) as never,
          verifyDomain: ((_id: number, _did: string) => emptyApi()) as never,
          deleteDomain: ((_id: number, _did: string) => Effect.void) as never,
          listRegistries: ((_pid: string) => emptyApi()) as never,
          getRegistry: ((_pid: string, _rid: string) => emptyApi()) as never,
          createRegistry: ((_pid: string, _input: never) => emptyApi()) as never,
          updateRegistry: ((_pid: string, _rid: string, _input: never) =>
            emptyApi()) as never,
          deleteRegistry: ((_pid: string, _rid: string) => Effect.void) as never,
          getOciCache: emptyApi() as never,
          runOciCacheGc: emptyApi() as never,
          getHostedRegistryStatus: emptyApi() as never,
          listHostedRepositories: emptyApi() as never,
          runHostedRegistryGc: emptyApi() as never,
        }),
      ),
    ),
  )
})
