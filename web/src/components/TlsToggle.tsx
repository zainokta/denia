import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import type { Service } from '#/effect/schema'
import { errorMessage } from './ErrorPanel'
import { useActionToasts } from './Toast'

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
  const toast = useActionToasts()
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
      // Also invalidate the detail-page query key so a toggle from the service
      // detail header refreshes the TLS row immediately.
      queryClient.invalidateQueries({ queryKey: ['services', service.id] })
      queryClient.invalidateQueries({ queryKey: ['ingress', 'routes'] })
      toast.ok(tlsEnabled ? 'TLS disabled' : 'TLS enabled')
    },
    onError: (err: unknown) => toast.err(errorMessage(err)),
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
