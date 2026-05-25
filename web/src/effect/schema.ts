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
