import { Schema } from 'effect'

export class Node extends Schema.Class<Node>('Node')({
  id: Schema.Number,
  name: Schema.String,
}) {}

export const Nodes = Schema.Array(Node)

export const Role = Schema.Literals(['viewer', 'operator', 'admin'])
export type Role = typeof Role.Type

export class User extends Schema.Class<User>('User')({
  id: Schema.Number,
  username: Schema.String,
  created_at: Schema.String,
}) {}

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
  project_id: Schema.Number,
  role: Role,
}) {}

export class Me extends Schema.Class<Me>('Me')({
  principal: PrincipalView,
  is_super_admin: Schema.Boolean,
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

export class Service extends Schema.Class<Service>('Service')({
  id: Schema.Number,
  project_id: Schema.Number,
  name: Schema.String,
  domains: Schema.Array(Schema.String),
  internal_port: Schema.Number,
  status: Schema.optional(Schema.String),
  tls_enabled: Schema.optional(Schema.Boolean),
  security: Schema.optional(SecurityPosture),
}) {}

export const Services = Schema.Array(Service)

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
  id: Schema.Number,
  service_id: Schema.Number,
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
  bridge_port: Schema.Number,
  tls: Schema.Boolean,
}) {}
export const RouteViews = Schema.Array(RouteView)

export const JobRunStatus = Schema.Literals([
  'Pending',
  'Running',
  'Succeeded',
  'Failed',
  'Skipped',
])
export type JobRunStatus = typeof JobRunStatus.Type

const GitSource = Schema.Struct({
  type: Schema.Literal('git'),
  repo_url: Schema.String,
  git_ref: Schema.String,
  dockerfile_path: Schema.String,
  context_path: Schema.String,
  credential: Schema.Struct({ name: Schema.String, key: Schema.String }),
})

const ExternalImageSource = Schema.Struct({
  type: Schema.Literal('external_image'),
  image: Schema.String,
  credential: Schema.NullOr(
    Schema.Struct({ name: Schema.String, key: Schema.String }),
  ),
})

const ServiceSource = Schema.Union([GitSource, ExternalImageSource])

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
