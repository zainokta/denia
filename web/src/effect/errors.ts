import { Schema } from 'effect'

export class ApiError extends Schema.TaggedErrorClass<ApiError>()('ApiError', {
  message: Schema.String,
  status: Schema.Number,
}) {}

export class DecodeError extends Schema.TaggedErrorClass<DecodeError>()(
  'DecodeError',
  {
    message: Schema.String,
  },
) {}
