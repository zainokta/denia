import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { Service } from '#/effect/schema'

interface Props {
  service: Service
}

const putService = (svc: Service) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.putService(svc)
  })

export function TlsToggle({ service }: Props) {
  const queryClient = useQueryClient()
  const tlsEnabled = service.tls_enabled ?? false

  const toggle = useMutation({
    mutationFn: () =>
      runQuery(
        putService({
          ...service,
          tls_enabled: !tlsEnabled,
        }),
      ),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      queryClient.invalidateQueries({ queryKey: ['ingress', 'routes'] })
    },
  })

  return (
    <div className="flex items-center gap-3">
      {tlsEnabled ? (
        <span className="inline-flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
          <span className="signal signal-steady" aria-hidden="true" />
          TLS
        </span>
      ) : (
        <span className="text-xs text-[var(--fg-muted)]">http</span>
      )}
      <button
        className="btn text-xs"
        type="button"
        onClick={() => toggle.mutate()}
        disabled={toggle.isPending}
      >
        {toggle.isPending
          ? 'updating...'
          : tlsEnabled
            ? 'Disable TLS'
            : 'Enable TLS'}
      </button>
    </div>
  )
}
