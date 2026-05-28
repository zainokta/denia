import { Effect, Schema } from 'effect'

export class Node extends Schema.Class<Node>('Node')({
  id: Schema.Number,
  name: Schema.String,
}) {}

export const Nodes = Schema.Array(Node)

export const Role = Schema.Literals(['viewer', 'operator', 'admin'])
export type Role = typeof Role.Type

export class User extends Schema.Class<User>('User')({
  id: Schema.String,
  username: Schema.String,
  created_at: Schema.String,
}) {}

export class UserSummary extends Schema.Class<UserSummary>('UserSummary')({
  id: Schema.String,
  username: Schema.String,
}) {}
export const UserSummaries = Schema.Array(UserSummary)

export const PrincipalUser = Schema.Struct({
  kind: Schema.Literal('user'),
  user: User,
})

export const PrincipalBootstrap = Schema.Struct({
  kind: Schema.Literal('bootstrap'),
})

export const PrincipalView = Schema.Union([
  PrincipalUser,
  PrincipalBootstrap,
])
export type PrincipalView = typeof PrincipalView.Type

export class Membership extends Schema.Class<Membership>('Membership')({
  project_id: Schema.String,
  role: Role,
}) {}

export class ProjectMember extends Schema.Class<ProjectMember>('ProjectMember')({
  user_id: Schema.String,
  project_id: Schema.String,
  role: Role,
}) {}
export const ProjectMembers = Schema.Array(ProjectMember)

export class Me extends Schema.Class<Me>('Me')({
  principal: PrincipalView,
  is_super_admin: Schema.Boolean,
  admin_initialized: Schema.Boolean,
  memberships: Schema.Array(Membership),
}) {}

export class LoginResult extends Schema.Class<LoginResult>('LoginResult')({
  token: Schema.String,
  expires_at: Schema.String,
}) {}

export class ApiToken extends Schema.Class<ApiToken>('ApiToken')({
  id: Schema.Number,
  name: Schema.String,
  token: Schema.String,
  created_at: Schema.String,
}) {}

export class Session extends Schema.Class<Session>('Session')({
  id: Schema.String,
  expires_at: Schema.String,
}) {}

export const Sessions = Schema.Array(Session)

export const SessionRevoke = Schema.Struct({ revoked: Schema.Number })

export const CredentialKind = Schema.Literals([
  'SshDeployKey',
  'RegistryBasic',
  'RegistryToken',
])
export type CredentialKind = typeof CredentialKind.Type

export class Credential extends Schema.Class<Credential>('Credential')({
  id: Schema.String,
  name: Schema.String,
  kind: CredentialKind,
  secret_ref: Schema.String,
}) {}

export const Credentials = Schema.Array(Credential)

export interface CredentialInput {
  readonly name: string
  readonly kind: CredentialKind
  readonly secret_ref: string
}

export class Project extends Schema.Class<Project>('Project')({
  id: Schema.String,
  name: Schema.String,
  description: Schema.NullOr(Schema.String),
  shared_env: Schema.Array(Schema.Struct({ key: Schema.String, value: Schema.String })),
  default_resource_limits: Schema.NullOr(
    Schema.Struct({ cpu_millis: Schema.Number, memory_bytes: Schema.Number }),
  ),
  created_at: Schema.String,
}) {}

export const Projects = Schema.Array(Project)

export class ProjectInput extends Schema.Class<ProjectInput>('ProjectInput')({
  name: Schema.String,
  description: Schema.NullOr(Schema.String),
  shared_env: Schema.Array(Schema.Struct({ key: Schema.String, value: Schema.String })),
  default_resource_limits: Schema.NullOr(
    Schema.Struct({ cpu_millis: Schema.Number, memory_bytes: Schema.Number }),
  ),
}) {}

export const SecurityPosture = Schema.Struct({
  userns: Schema.Boolean,
  mapped_uid: Schema.NullOr(Schema.Number),
  no_new_privs: Schema.Boolean,
  caps_dropped: Schema.Boolean,
})

export const DeploymentStatus = Schema.Literals([
  'Pending',
  'Building',
  'Starting',
  'Healthy',
  'Failed',
  'Stopped',
])

export const ArtifactRef = Schema.Struct({
  digest: Schema.String,
  kind: Schema.Literals(['OciImage', 'RootfsBundle']),
})

export class Deployment extends Schema.Class<Deployment>('Deployment')({
  id: Schema.String,
  service_id: Schema.String,
  status: Schema.String,
  created_at: Schema.String,
  artifact: Schema.optional(ArtifactRef),
}) {}

export const Deployments = Schema.Array(Deployment)

export class MetricSnapshot extends Schema.Class<MetricSnapshot>('MetricSnapshot')({
  service_id: Schema.Number,
  cpu_percent: Schema.Number,
  memory_bytes: Schema.Number,
  recorded_at: Schema.String,
}) {}

export const Metrics = Schema.Array(MetricSnapshot)

export class RouteView extends Schema.Class<RouteView>('RouteView')({
  service_name: Schema.String,
  domains: Schema.Array(Schema.String),
  tls: Schema.Boolean,
}) {}
export const RouteViews = Schema.Array(RouteView)

export const CpuCounters = Schema.Struct({
  user_jiffies: Schema.Number,
  nice_jiffies: Schema.Number,
  system_jiffies: Schema.Number,
  idle_jiffies: Schema.Number,
  iowait_jiffies: Schema.Number,
})
export type CpuCounters = typeof CpuCounters.Type

export class NodeSnapshot extends Schema.Class<NodeSnapshot>('NodeSnapshot')({
  cpu: CpuCounters,
  memory_total_bytes: Schema.Number,
  memory_available_bytes: Schema.Number,
  load_1m: Schema.Number,
  load_5m: Schema.Number,
  load_15m: Schema.Number,
  disk_total_bytes: Schema.Number,
  disk_available_bytes: Schema.Number,
  recorded_at: Schema.String,
}) {}

export class WorkloadView extends Schema.Class<WorkloadView>('WorkloadView')({
  service_id: Schema.String,
  service_name: Schema.String,
  project_id: Schema.String,
  deployment_id: Schema.NullOr(Schema.String),
  status: Schema.NullOr(DeploymentStatus),
  cpu_usage_usec: Schema.NullOr(Schema.Number),
  memory_current_bytes: Schema.NullOr(Schema.Number),
}) {}
export const WorkloadViews = Schema.Array(WorkloadView)

export class AccessEntry extends Schema.Class<AccessEntry>('AccessEntry')({
  service_name: Schema.String,
  method: Schema.String,
  path: Schema.String,
  status: Schema.Number,
  bytes: Schema.NullOr(Schema.Number),
  duration_ms: Schema.NullOr(Schema.Number),
  recorded_at: Schema.String,
}) {}
export const AccessEntries = Schema.Array(AccessEntry)

export const JobRunStatus = Schema.Literals([
  'Pending',
  'Running',
  'Succeeded',
  'Failed',
  'Skipped',
])
export type JobRunStatus = typeof JobRunStatus.Type

export const GitSource = Schema.Struct({
  type: Schema.Literal('git'),
  repo_url: Schema.String,
  git_ref: Schema.String,
  dockerfile_path: Schema.String,
  context_path: Schema.String,
  credential: Schema.Struct({ name: Schema.String, key: Schema.String }),
})
export type GitSource = typeof GitSource.Type

export const ExternalImageSource = Schema.Struct({
  type: Schema.Literal('external_image'),
  image: Schema.String,
  credential: Schema.NullOr(
    Schema.Struct({ name: Schema.String, key: Schema.String }),
  ),
  registry_id: Schema.NullOr(Schema.String),
  image_ref: Schema.NullOr(Schema.String),
})
export type ExternalImageSource = typeof ExternalImageSource.Type

export const ServiceSource = Schema.Union([GitSource, ExternalImageSource])

export const HealthCheck = Schema.Struct({
  path: Schema.String,
  timeout_seconds: Schema.Number,
})

export const ResourceLimits = Schema.Struct({
  cpu_millis: Schema.Number,
  memory_bytes: Schema.Number,
})

export class Service extends Schema.Class<Service>('Service')({
  id: Schema.String,
  project_id: Schema.String,
  name: Schema.String,
  domains: Schema.Array(Schema.String),
  source: ServiceSource,
  internal_port: Schema.Number,
  health_check: HealthCheck,
  resource_limits: Schema.optional(Schema.NullOr(ResourceLimits)),
  env: Schema.Array(Schema.Tuple([Schema.String, Schema.String])),
  tls_enabled: Schema.optionalKey(Schema.Boolean).pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
}) {}

export const Services = Schema.Array(Service)

// Service fields minus `id`, for create requests.
const { id: _serviceId, ...serviceInputFields } = Service.fields
export const ServiceInput = Schema.Struct(serviceInputFields)
export type ServiceInput = typeof ServiceInput.Type

export class Job extends Schema.Class<Job>('Job')({
  id: Schema.String,
  project_id: Schema.String,
  name: Schema.String,
  source: ServiceSource,
  command: Schema.NullOr(Schema.Array(Schema.String)),
  env: Schema.Array(Schema.Tuple([Schema.String, Schema.String])),
  schedule: Schema.NullOr(Schema.String),
  max_retries: Schema.Number,
  next_run_at: Schema.NullOr(Schema.String),
  last_enqueued_at: Schema.NullOr(Schema.String),
  created_at: Schema.String,
}) {}

export const Jobs = Schema.Array(Job)

export class JobRun extends Schema.Class<JobRun>('JobRun')({
  id: Schema.String,
  job_id: Schema.String,
  status: JobRunStatus,
  attempt: Schema.Number,
  exit_code: Schema.NullOr(Schema.Number),
  started_at: Schema.NullOr(Schema.String),
  finished_at: Schema.NullOr(Schema.String),
  created_at: Schema.String,
}) {}

export const JobRuns = Schema.Array(JobRun)

export class ServiceDomain extends Schema.Class<ServiceDomain>('ServiceDomain')({
  id: Schema.String,
  hostname: Schema.String,
  status: Schema.Literals(['verified', 'pending', 'failed']),
  verified_at: Schema.NullOr(Schema.String),
  last_error: Schema.NullOr(Schema.String),
  created_at: Schema.String,
}) {}

export const ServiceDomains = Schema.Array(ServiceDomain)

export const RegistryAuthKind = Schema.Literals([
  'anonymous',
  'basic',
  'token',
  'ecr_token',
  'gar_token',
])
export type RegistryAuthKind = typeof RegistryAuthKind.Type

export class Registry extends Schema.Class<Registry>('Registry')({
  id: Schema.String,
  project_id: Schema.String,
  name: Schema.String,
  endpoint: Schema.String,
  auth_kind: RegistryAuthKind,
  credential_ref: Schema.NullOr(Schema.String),
}) {}

export const Registries = Schema.Array(Registry)

export class RegistryInput extends Schema.Class<RegistryInput>('RegistryInput')({
  name: Schema.String,
  endpoint: Schema.String,
  auth_kind: RegistryAuthKind,
  secret_ref: Schema.NullOr(Schema.String),
}) {}
