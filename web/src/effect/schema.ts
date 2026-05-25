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

export class Service extends Schema.Class<Service>('Service')({
  id: Schema.Number,
  project_id: Schema.Number,
  name: Schema.String,
  domains: Schema.Array(Schema.String),
  internal_port: Schema.Number,
  status: Schema.optional(Schema.String),
  tls_enabled: Schema.optional(Schema.Boolean),
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

export class Deployment extends Schema.Class<Deployment>('Deployment')({
  id: Schema.Number,
  service_id: Schema.Number,
  status: Schema.String,
  created_at: Schema.String,
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
