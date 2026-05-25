import { Schema } from 'effect'

export class Node extends Schema.Class<Node>('Node')({
  id: Schema.Number,
  name: Schema.String,
}) {}

export const Nodes = Schema.Array(Node)

export class Project extends Schema.Class<Project>('Project')({
  id: Schema.String,
  name: Schema.String,
  description: Schema.NullOr(Schema.String),
  shared_env: Schema.Array(Schema.Tuple([Schema.String, Schema.String])),
  default_resource_limits: Schema.NullOr(
    Schema.Struct({ cpu_millis: Schema.Number, memory_bytes: Schema.Number }),
  ),
  created_at: Schema.String,
}) {}

export const Projects = Schema.Array(Project)
