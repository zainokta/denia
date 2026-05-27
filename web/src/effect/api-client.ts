import { Context, Effect, Layer, Schema } from 'effect'
import { HttpClient, HttpBody } from 'effect/unstable/http'
import { AppConfig } from './config'
import { ApiError, DecodeError } from './errors'
import {
  AccessEntries,
  AccessEntry,
  ApiToken,
  Deployment,
  Deployments,
  Job,
  JobRun,
  JobRuns,
  Jobs,
  LoginResult,
  Me,
  MetricSnapshot,
  Metrics,
  Node,
  NodeSnapshot,
  Nodes,
  Project,
  ProjectInput,
  ProjectMember,
  ProjectMembers,
  Projects,
  Registries,
  Registry,
  RegistryInput,
  type Role,
  RouteView,
  RouteViews,
  Service,
  ServiceInput,
  ServiceDomain,
  ServiceDomains,
  Services,
  User,
  WorkloadView,
  WorkloadViews,
} from './schema'

export class ApiClient extends Context.Service<
  ApiClient,
  {
    readonly listNodes: Effect.Effect<
      ReadonlyArray<Node>,
      ApiError | DecodeError
    >
    readonly login: (
      username: string,
      password: string,
    ) => Effect.Effect<LoginResult, ApiError | DecodeError>
    readonly logout: Effect.Effect<void>
    readonly me: Effect.Effect<Me, ApiError | DecodeError>
    readonly listUsers: Effect.Effect<
      ReadonlyArray<User>,
      ApiError | DecodeError
    >
    readonly createUser: (
      username: string,
      password: string,
    ) => Effect.Effect<User, ApiError | DecodeError>
    readonly bootstrap: (
      username: string,
      password: string,
    ) => Effect.Effect<User, ApiError | DecodeError>
    readonly deleteUser: (id: string) => Effect.Effect<void, ApiError>
    readonly listApiTokens: Effect.Effect<
      ReadonlyArray<ApiToken>,
      ApiError | DecodeError
    >
    readonly createApiToken: (
      name: string,
    ) => Effect.Effect<ApiToken, ApiError | DecodeError>
    readonly deleteApiToken: (id: number) => Effect.Effect<void, ApiError>
    readonly listMembers: (
      projectId: string,
    ) => Effect.Effect<ReadonlyArray<ProjectMember>, ApiError | DecodeError>
    readonly addMember: (
      projectId: string,
      userId: string,
      role: Role,
    ) => Effect.Effect<ProjectMember, ApiError | DecodeError>
    readonly removeMember: (
      projectId: string,
      userId: string,
    ) => Effect.Effect<void, ApiError>
    readonly listServices: Effect.Effect<
      ReadonlyArray<Service>,
      ApiError | DecodeError
    >
    readonly getService: (
      id: string,
    ) => Effect.Effect<Service, ApiError | DecodeError>
    readonly deleteService: (id: string) => Effect.Effect<void, ApiError>
    readonly getServiceDeployments: (
      id: string,
    ) => Effect.Effect<
      ReadonlyArray<Deployment>,
      ApiError | DecodeError
    >
    readonly getServiceLogs: (
      id: string,
    ) => Effect.Effect<ReadonlyArray<string>, ApiError | DecodeError>
    readonly getServiceMetrics: (
      id: string,
    ) => Effect.Effect<
      ReadonlyArray<MetricSnapshot>,
      ApiError | DecodeError
    >
    readonly createDeployment: (
      service: Service,
    ) => Effect.Effect<Deployment, ApiError | DecodeError>
    readonly stopService: (
      id: string,
    ) => Effect.Effect<void, ApiError>
    readonly listProjects: Effect.Effect<
      ReadonlyArray<Project>,
      ApiError | DecodeError
    >
    readonly getProject: (
      id: string,
    ) => Effect.Effect<Project, ApiError | DecodeError>
    readonly createProject: (
      input: ProjectInput,
    ) => Effect.Effect<Project, ApiError | DecodeError>
    readonly deleteProject: (
      id: string,
    ) => Effect.Effect<void, ApiError>
    readonly putService: (
      service: Service | ServiceInput,
    ) => Effect.Effect<Service, ApiError | DecodeError>
    readonly listRoutes: Effect.Effect<
      ReadonlyArray<RouteView>,
      ApiError | DecodeError
    >
    readonly getIngressConfig: Effect.Effect<string, ApiError>
    readonly listJobs: (
      projectId: string,
    ) => Effect.Effect<ReadonlyArray<Job>, ApiError | DecodeError>
    readonly getJob: (
      id: string,
    ) => Effect.Effect<Job, ApiError | DecodeError>
    readonly createJob: (
      input: Job,
    ) => Effect.Effect<Job, ApiError | DecodeError>
    readonly deleteJob: (id: string) => Effect.Effect<void, ApiError>
    readonly runJob: (
      id: string,
    ) => Effect.Effect<JobRun, ApiError | DecodeError>
    readonly listJobRuns: (
      id: string,
    ) => Effect.Effect<
      ReadonlyArray<JobRun>,
      ApiError | DecodeError
    >
    readonly getNodeMetrics: Effect.Effect<
      NodeSnapshot,
      ApiError | DecodeError
    >
    readonly listWorkloads: Effect.Effect<
      ReadonlyArray<WorkloadView>,
      ApiError | DecodeError
    >
    readonly listServiceRequests: (
      id: string,
    ) => Effect.Effect<
      ReadonlyArray<AccessEntry>,
      ApiError | DecodeError
    >
    readonly listDomains: (
      serviceId: string,
    ) => Effect.Effect<
      ReadonlyArray<ServiceDomain>,
      ApiError | DecodeError
    >
    readonly addDomain: (
      serviceId: string,
      hostname: string,
    ) => Effect.Effect<ServiceDomain, ApiError | DecodeError>
    readonly verifyDomain: (
      serviceId: string,
      domainId: string,
    ) => Effect.Effect<ServiceDomain, ApiError | DecodeError>
    readonly deleteDomain: (
      serviceId: string,
      domainId: string,
    ) => Effect.Effect<void, ApiError>
    readonly listRegistries: (
      projectId: string,
    ) => Effect.Effect<
      ReadonlyArray<Registry>,
      ApiError | DecodeError
    >
    readonly createRegistry: (
      projectId: string,
      input: RegistryInput,
    ) => Effect.Effect<Registry, ApiError | DecodeError>
    readonly deleteRegistry: (
      projectId: string,
      registryId: string,
    ) => Effect.Effect<void, ApiError>
  }
>()('ApiClient') {}

function decode<A>(schema: Schema.Schema<A>) {
  return (input: unknown) =>
    Schema.decodeUnknownEffect(schema)(input).pipe(
      Effect.mapError(
        (error) => new DecodeError({ message: String(error) }),
      ),
    )
}

function httpError(error: unknown): ApiError {
  return new ApiError({ message: String(error), status: 0 })
}

function unauthorized(): ApiError {
  return new ApiError({ message: 'Unauthorized', status: 401 })
}

function forbidden(): ApiError {
  return new ApiError({ message: 'Forbidden', status: 403 })
}

function jsonBody(obj: unknown) {
  return HttpBody.jsonUnsafe(obj)
}

function parseResponse<A>(
  response: { readonly status: number; readonly json: Effect.Effect<unknown, unknown> },
  schema: Schema.Schema<A>,
): Effect.Effect<A, ApiError | DecodeError> {
  return Effect.gen(function* () {
    if (response.status === 401)
      return yield* Effect.fail(unauthorized())
    if (response.status === 403)
      return yield* Effect.fail(forbidden())
    const body = yield* (response.json as Effect.Effect<unknown, ApiError>).pipe(
      Effect.mapError(httpError),
    )
    if (response.status < 200 || response.status >= 300)
      return yield* Effect.fail(
        new ApiError({
          message:
            typeof (body as Record<string, unknown>).message === 'string'
              ? String((body as Record<string, unknown>).message)
              : `HTTP ${response.status}`,
          status: response.status,
        }),
      )
    return yield* decode(schema)(body)
  }) as Effect.Effect<A, ApiError | DecodeError>
}

function parseDeleteResponse(
  response: { readonly status: number; readonly json: Effect.Effect<unknown, unknown> },
): Effect.Effect<void, ApiError> {
  return Effect.gen(function* () {
    if (response.status === 401)
      return yield* Effect.fail(unauthorized())
    if (response.status === 403)
      return yield* Effect.fail(forbidden())
    if (response.status < 200 || response.status >= 300) {
      const body = yield* (response.json as Effect.Effect<unknown, ApiError>).pipe(
        Effect.mapError(httpError),
      )
      return yield* Effect.fail(
        new ApiError({
          message:
            typeof (body as Record<string, unknown>).message === 'string'
              ? String((body as Record<string, unknown>).message)
              : `HTTP ${response.status}`,
          status: response.status,
        }),
      )
    }
  }) as Effect.Effect<void, ApiError>
}

function parseTextResponse(
  response: { readonly status: number; readonly text: Effect.Effect<string, unknown> },
): Effect.Effect<string, ApiError> {
  return Effect.gen(function* () {
    if (response.status === 401)
      return yield* Effect.fail(unauthorized())
    if (response.status === 403)
      return yield* Effect.fail(forbidden())
    const body = yield* (response.text as Effect.Effect<string, ApiError>).pipe(
      Effect.mapError(httpError),
    )
    if (response.status < 200 || response.status >= 300)
      return yield* Effect.fail(
        new ApiError({ message: body, status: response.status }),
      )
    return body
  }) as Effect.Effect<string, ApiError>
}

export const ApiClientLive = Layer.effect(ApiClient)(
  Effect.gen(function* () {
    const config = yield* AppConfig
    const http = yield* HttpClient.HttpClient

    const authHeaders = () => {
      const token = config.getAuthToken()
      return token ? { authorization: `Bearer ${token}` } : {}
    }

    const url = (path: string) => `${config.baseUrl}${path}`

    const listNodes = Effect.gen(function* () {
      const headers = authHeaders()
      const response = yield* http
        .get(url('/v1/nodes'), { headers })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Nodes)
    })

    const login = (username: string, password: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/auth/login'), {
            headers: { 'content-type': 'application/json' },
            body: jsonBody({ username, password }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, LoginResult)
      })

    const logout = Effect.gen(function* () {
      const token = config.getAuthToken()
      if (!token) return
      yield* http
        .post(url('/v1/auth/logout'), {
          headers: { authorization: `Bearer ${token}` },
        })
        .pipe(Effect.mapError(httpError), Effect.ignore)
    })

    const me = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/me'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Me)
    })

    const listUsers = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/users'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Schema.Array(User))
    })

    const createUser = (username: string, password: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/users'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ username, password }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, User)
      })

    const bootstrap = (username: string, password: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/bootstrap'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ username, password }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, User)
      })

    const deleteUser = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/users/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const listApiTokens = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/api-tokens'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Schema.Array(ApiToken))
    })

    const createApiToken = (name: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/api-tokens'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ name }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ApiToken)
      })

    const deleteApiToken = (id: number) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/api-tokens/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const listMembers = (projectId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/projects/${projectId}/members`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ProjectMembers)
      })

    const addMember = (projectId: string, userId: string, role: Role) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url(`/v1/projects/${projectId}/members`), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ user_id: userId, role }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ProjectMember)
      })

    const removeMember = (projectId: string, userId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/projects/${projectId}/members/${userId}`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const listServices = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/services'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Services)
    })

    const getServiceDeployments = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${id}/deployments`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Deployments)
      })

    const getServiceLogs = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${id}/logs`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Schema.Array(Schema.String))
      })

    const getServiceMetrics = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${id}/metrics`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Metrics)
      })

    const createDeployment = (service: Service) =>
      Effect.gen(function* () {
        const src = service.source
        const body =
          src.type === 'git'
            ? {
                source: 'git',
                service_id: service.id,
                repo_url: src.repo_url,
                git_ref: src.git_ref,
              }
            : {
                source: 'external_image',
                service_id: service.id,
                image: src.image,
              }
        const response = yield* http
          .post(url('/v1/deployments'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody(body),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Deployment)
      })

    const stopService = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url(`/v1/services/${id}/stop`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const listProjects = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/projects'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, Projects)
    })

    const getProject = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/projects/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Project)
      })

    const createProject = (input: ProjectInput) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/projects'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody(input),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Project)
      })

    const deleteProject = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/projects/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const getService = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Service)
      })

    const deleteService = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/services/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const putService = (service: Service | ServiceInput) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/services'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody(service),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Service)
      })

    const listRoutes = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/ingress/routes'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, RouteViews)
    })

    const getIngressConfig = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/ingress/config'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseTextResponse(response)
    })

    const listJobs = (projectId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/jobs?project_id=${projectId}`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Jobs)
      })

    const getJob = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/jobs/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Job)
      })

    const createJob = (input: Job) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/jobs'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody(input),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Job)
      })

    const deleteJob = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(url(`/v1/jobs/${id}`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const runJob = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url(`/v1/jobs/${id}/run`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, JobRun)
      })

    const listJobRuns = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/jobs/${id}/runs`), { headers: authHeaders() })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, JobRuns)
      })

    const getNodeMetrics = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/metrics/node'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, NodeSnapshot)
    })

    const listWorkloads = Effect.gen(function* () {
      const response = yield* http
        .get(url('/v1/workloads'), { headers: authHeaders() })
        .pipe(Effect.mapError(httpError))
      return yield* parseResponse(response, WorkloadViews)
    })

    const listServiceRequests = (id: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${id}/requests`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, AccessEntries)
      })

    const listDomains = (serviceId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/services/${serviceId}/domains`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ServiceDomains)
      })

    const addDomain = (serviceId: string, hostname: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url(`/v1/services/${serviceId}/domains`), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ hostname }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ServiceDomain)
      })

    const verifyDomain = (serviceId: string, domainId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(
            url(`/v1/services/${serviceId}/domains/${domainId}/verify`),
            { headers: authHeaders() },
          )
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, ServiceDomain)
      })

    const deleteDomain = (serviceId: string, domainId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(
            url(`/v1/services/${serviceId}/domains/${domainId}`),
            { headers: authHeaders() },
          )
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    const listRegistries = (projectId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .get(url(`/v1/projects/${projectId}/registries`), {
            headers: authHeaders(),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Registries)
      })

    const createRegistry = (projectId: string, input: RegistryInput) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url(`/v1/projects/${projectId}/registries`), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody(input),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, Registry)
      })

    const deleteRegistry = (projectId: string, registryId: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .del(
            url(`/v1/projects/${projectId}/registries/${registryId}`),
            { headers: authHeaders() },
          )
          .pipe(Effect.mapError(httpError))
        return yield* parseDeleteResponse(response)
      })

    return {
      listNodes,
      login,
      logout,
      me,
      listUsers,
      createUser,
      bootstrap,
      deleteUser,
      listApiTokens,
      createApiToken,
      deleteApiToken,
      listMembers,
      addMember,
      removeMember,
      listServices,
      getService,
      deleteService,
      getServiceDeployments,
      getServiceLogs,
      getServiceMetrics,
      createDeployment,
      stopService,
      listProjects,
      getProject,
      createProject,
      deleteProject,
      putService,
      listRoutes,
      getIngressConfig,
      listJobs,
      getJob,
      createJob,
      deleteJob,
      runJob,
      listJobRuns,
      getNodeMetrics,
      listWorkloads,
      listServiceRequests,
      listDomains,
      addDomain,
      verifyDomain,
      deleteDomain,
      listRegistries,
      createRegistry,
      deleteRegistry,
    }
  }),
)
