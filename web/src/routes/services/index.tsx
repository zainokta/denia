import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { StatusSignal } from '#/components/StatusSignal'
import { SecurityBadge } from '#/components/SecurityBadge'
import { useAuth, can } from '#/hooks/useAuth'

const listServices = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listServices
})

const createDeployment = (input: {
  service_id: number
  source?: {
    type: 'git'
    repo_url: string
    git_ref: string
    dockerfile_path: string
    context_path: string
    credential?: { name: string; key: string }
  } | {
    type: 'external_image'
    image?: string
    registry_id?: string
    image_ref?: string
    credential?: { name: string; key: string } | null
  }
}) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createDeployment(input)
  })

const stopService = (id: number) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.stopService(id)
  })

export const Route = createFileRoute('/services/')({
  component: ServicesIndex,
})

export function ServicesIndex() {
  const queryClient = useQueryClient()
  const { isSuperAdmin, roleForActiveProject } = useAuth()

  // Deployment source form state
  const [createOpen, setCreateOpen] = useState(false)
  const [sourceType, setSourceType] = useState<string>('git')
  const [gitRepoUrl, setGitRepoUrl] = useState('')
  const [gitRef, setGitRef] = useState('main')
  const [gitDockerfilePath, setGitDockerfilePath] = useState('Dockerfile')
  const [gitContextPath, setGitContextPath] = useState('.')
  const [extImage, setExtImage] = useState('')
  const [extRegistryId, setExtRegistryId] = useState('')
  const [extImageRef, setExtImageRef] = useState('')
  const [createError, setCreateError] = useState('')

  const { data: services = [], isFetching } = useQuery({
    queryKey: ['services'],
    queryFn: () => runQuery(listServices),
  })

  const canOperate = (projectId: number): boolean => {
    if (isSuperAdmin) return true
    const role = roleForActiveProject(String(projectId))
    return role !== undefined && can('operator', role)
  }

  const canOperateService = (): boolean => {
    if (isSuperAdmin) return true
    // Check if user has operator+ role on any project
    for (const svc of services) {
      if (canOperate(svc.project_id)) return true
    }
    return false
  }

  const buildSource = () => {
    if (sourceType === 'git') {
      return {
        type: 'git' as const,
        repo_url: gitRepoUrl.trim(),
        git_ref: gitRef.trim() || 'main',
        dockerfile_path: gitDockerfilePath.trim() || 'Dockerfile',
        context_path: gitContextPath.trim() || '.',
      }
    }
    // external_image — validate: must have legacy image OR (registry_id + image_ref)
    const hasLegacy = extImage.trim().length > 0
    const hasNew =
      extRegistryId.trim().length > 0 && extImageRef.trim().length > 0
    if (!hasLegacy && !hasNew) return undefined

    return {
      type: 'external_image' as const,
      image: hasLegacy ? extImage.trim() : undefined,
      registry_id: hasNew ? extRegistryId.trim() : undefined,
      image_ref: hasNew ? extImageRef.trim() : undefined,
    }
  }

  const deploy = useMutation({
    mutationFn: (input: {
      id: number
      source?: Parameters<typeof createDeployment>[0]['source']
    }) => runQuery(createDeployment({ service_id: input.id, source: input.source })),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
      setCreateError('')
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : 'Deploy failed'
      setCreateError(msg)
    },
  })

  const stop = useMutation({
    mutationFn: (id: number) => runQuery(stopService(id)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['services'] })
    },
  })

  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <p className="kicker mb-3">services</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Services
      </h1>

      {/* Deployment source form — collapsible panel */}
      {canOperateService() ? (
        <section className="panel mb-6 overflow-hidden">
          <button
            type="button"
            className="flex w-full items-center justify-between px-4 py-2.5 text-left text-sm font-semibold text-[var(--fg)] hover:bg-[var(--surface-2)]"
            onClick={() => setCreateOpen((v) => !v)}
          >
            <span className="flex items-center gap-2">
              <span className="signal signal-steady" aria-hidden="true" />
              new deployment
            </span>
            <span className="text-xs text-[var(--fg-muted)]">
              {createOpen ? '▲' : '▼'}
            </span>
          </button>

          {createOpen ? (
            <div className="border-t border-[var(--border)] px-4 py-3">
              {createError ? (
                <div className="mb-3 text-xs text-[var(--violet)]">
                  <span className="signal signal-fault mr-2 inline-block align-middle" />
                  {createError}
                </div>
              ) : null}

              <fieldset className="mb-3 flex flex-wrap items-center gap-4 text-sm">
                <legend className="text-xs text-[var(--fg-muted)] mb-1">
                  source type
                </legend>
                <label className="inline-flex items-center gap-1.5 text-[var(--fg)]">
                  <input
                    type="radio"
                    name="sourceType"
                    value="git"
                    checked={sourceType === 'git'}
                    onChange={() => setSourceType('git')}
                  />
                  Git
                </label>
                <label className="inline-flex items-center gap-1.5 text-[var(--fg)]">
                  <input
                    type="radio"
                    name="sourceType"
                    value="external_image"
                    checked={sourceType === 'external_image'}
                    onChange={() => setSourceType('external_image')}
                  />
                  External Image
                </label>
              </fieldset>

              {sourceType === 'git' ? (
                <div className="flex flex-wrap items-end gap-2 mb-3">
                  <input
                    type="text"
                    placeholder="repo url"
                    value={gitRepoUrl}
                    onChange={(e) => setGitRepoUrl(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)] w-72"
                  />
                  <input
                    type="text"
                    placeholder="branch/tag"
                    value={gitRef}
                    onChange={(e) => setGitRef(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]"
                  />
                  <input
                    type="text"
                    placeholder="Dockerfile path"
                    value={gitDockerfilePath}
                    onChange={(e) => setGitDockerfilePath(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]"
                  />
                  <input
                    type="text"
                    placeholder="context path"
                    value={gitContextPath}
                    onChange={(e) => setGitContextPath(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]"
                  />
                </div>
              ) : (
                <div className="flex flex-wrap items-end gap-2 mb-3">
                  <input
                    type="text"
                    placeholder="image (legacy)"
                    value={extImage}
                    onChange={(e) => setExtImage(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)] w-72"
                  />
                  <span className="text-xs text-[var(--fg-muted)]">or</span>
                  <input
                    type="text"
                    placeholder="registry id"
                    value={extRegistryId}
                    onChange={(e) => setExtRegistryId(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]"
                  />
                  <input
                    type="text"
                    placeholder="image ref"
                    value={extImageRef}
                    onChange={(e) => setExtImageRef(e.target.value)}
                    className="border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]"
                  />
                </div>
              )}
              <p className="text-xs text-[var(--fg-muted)] mb-3">
                Select a service below and click <strong>deploy</strong> to
                use this source configuration.
              </p>
            </div>
          ) : null}
        </section>
      ) : null} 

      {services.length === 0 && !isFetching ? (
        <p className="text-[var(--fg-muted)]">
          No services yet. Create a project and deploy your first service.
        </p>
      ) : (
        <section className="panel overflow-hidden">
          <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
            <p className="kicker m-0">
              {isFetching ? 'fetching...' : `${services.length} service${services.length !== 1 ? 's' : ''}`}
            </p>
          </div>
          <ul className="m-0 list-none">
            {services.map((svc, i) => (
              <li
                key={svc.id}
                className={`flex items-center gap-4 px-4 py-3 text-sm ${
                  i > 0 ? 'border-t border-[var(--border)]' : ''
                }`}
              >
                {svc.status ? <StatusSignal status={svc.status} /> : null}
                <SecurityBadge security={svc.security} />
                <a
                  href={`/services/${svc.id}`}
                  className="min-w-0 flex-1 text-[var(--fg)] no-underline hover:underline"
                >
                  <span className="font-semibold">{svc.name}</span>
                  <span className="ml-3 text-xs text-[var(--fg-muted)]">
                    {svc.domains.join(', ') || `:${svc.internal_port}`}
                  </span>
                </a>
                {canOperate(svc.project_id) ? (
                  <>
                    <button
                      className="btn btn-primary text-xs"
                      type="button"
                      onClick={() => {
                        const source = createOpen ? buildSource() : undefined
                        deploy.mutate({ id: svc.id, source })
                      }}
                      disabled={deploy.isPending}
                    >
                      {deploy.isPending ? 'deploying...' : 'deploy'}
                    </button>
                    <button
                      className="btn text-xs"
                      type="button"
                      onClick={() => stop.mutate(svc.id)}
                      disabled={stop.isPending}
                    >
                      stop
                    </button>
                  </>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      )}
    </main>
  )
}
