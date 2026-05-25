import { Schema } from 'effect'

export class ApiError extends Schema.TaggedErrorClass<ApiError>()('ApiError', {
  message: Schema.String,
}) {}

export class DecodeError extends Schema.TaggedErrorClass<DecodeError>()(
  'DecodeError',
  {
    message: Schema.String,
  },
) {}
