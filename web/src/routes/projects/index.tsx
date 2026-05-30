import { createFileRoute, Link } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Effect } from 'effect'
import { FolderTree, Plus, Trash2 } from 'lucide-react'
import { ApiClient } from '#/effect/api-client'
import { runQuery } from '#/effect/runtime'
import { useActiveProject } from '#/hooks/useActiveProject'
import { EmptyState } from '#/components/EmptyState'
import { SkeletonRows } from '#/components/Skeleton'
import { InlineError, errorMessage } from '#/components/ErrorPanel'
import { useActionToasts } from '#/components/Toast'
import { Modal } from '#/components/Modal'
import { Num } from '#/components/Num'
import { formatBytes } from '#/lib/format'
import type { ProjectInput } from '#/effect/schema'

// Structural shape accepted by ApiClient.createProject (ProjectInput Schema.Class
// fields). Built as a plain literal so we don't depend on a class constructor.
type ProjectInputShape = {
  readonly name: string
  readonly description: string | null
  readonly shared_env: ReadonlyArray<{ key: string; value: string }>
  readonly default_resource_limits: {
    cpu_millis: number
    memory_bytes: number
  } | null
}

const listProjects = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listProjects
})

const createProject = (input: ProjectInputShape) =>
  Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createProject(input as ProjectInput)
  })

export const Route = createFileRoute('/projects/')({
  component: ProjectsIndex,
})

interface EnvRow {
  readonly key: string
  readonly value: string
}

export function ProjectsIndex() {
  const queryClient = useQueryClient()
  const toast = useActionToasts()
  const [, setActiveProject] = useActiveProject()

  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [sharedEnv, setSharedEnv] = useState<ReadonlyArray<EnvRow>>([])
  const [cpuMillis, setCpuMillis] = useState('')
  const [memoryBytes, setMemoryBytes] = useState('')
  const [createError, setCreateError] = useState('')

  const { data: projects = [], isLoading } = useQuery({
    queryKey: ['projects'],
    queryFn: () => runQuery(listProjects),
  })

  const resetForm = () => {
    setName('')
    setDescription('')
    setSharedEnv([])
    setCpuMillis('')
    setMemoryBytes('')
    setCreateError('')
  }

  const openModal = () => {
    resetForm()
    setOpen(true)
  }

  const create = useMutation({
    mutationFn: (input: ProjectInputShape) => runQuery(createProject(input)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      toast.ok('Project created')
      resetForm()
      setOpen(false)
    },
    onError: (error: unknown) => {
      const msg = errorMessage(error) || 'Failed to create project'
      setCreateError(msg)
      toast.err(msg)
    },
  })

  const handleCreate = (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return
    setCreateError('')

    // Only persist env rows with a non-empty key. Values may be blank.
    const cleanedEnv = sharedEnv
      .map((row) => ({ key: row.key.trim(), value: row.value }))
      .filter((row) => row.key.length > 0)

    // Limits are optional: null unless both numbers are present and valid.
    const cpu = cpuMillis.trim()
    const mem = memoryBytes.trim()
    let limits: ProjectInputShape['default_resource_limits'] = null
    if (cpu.length > 0 || mem.length > 0) {
      const cpuNum = Number(cpu)
      const memNum = Number(mem)
      if (
        cpu.length === 0 ||
        mem.length === 0 ||
        !Number.isFinite(cpuNum) ||
        !Number.isFinite(memNum)
      ) {
        setCreateError(
          'Set both CPU millis and memory bytes, or leave both blank.',
        )
        return
      }
      limits = { cpu_millis: cpuNum, memory_bytes: memNum }
    }

    create.mutate({
      name: name.trim(),
      description: description.trim() || null,
      shared_env: cleanedEnv,
      default_resource_limits: limits,
    })
  }

  const addEnvRow = () =>
    setSharedEnv((rows) => [...rows, { key: '', value: '' }])

  const updateEnvRow = (index: number, patch: Partial<EnvRow>) =>
    setSharedEnv((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    )

  const removeEnvRow = (index: number) =>
    setSharedEnv((rows) => rows.filter((_, i) => i !== index))

  const memNum = Number(memoryBytes)
  const memPretty =
    memoryBytes.trim() && Number.isFinite(memNum) && memNum > 0
      ? formatBytes(memNum)
      : null

  return (
    <main className="page-wrap px-4 pb-16 pt-10">
      <header className="panel-head">
        <div>
          <p className="kicker">control plane</p>
          <h1 className="t-display">Projects</h1>
        </div>
        <div className="cluster">
          {projects.length > 0 ? (
            <span className="badge">
              <Num>{projects.length}</Num>{' '}
              {projects.length === 1 ? 'project' : 'projects'}
            </span>
          ) : null}
          <button type="button" className="btn btn-primary" onClick={openModal}>
            <Plus size={14} aria-hidden="true" /> New project
          </button>
        </div>
      </header>

      <p className="text-faint" style={{ margin: '0.25rem 0 1.5rem', maxWidth: '64ch' }}>
        A project scopes services, members, registries, and a shared environment.
      </p>

      {isLoading ? (
        <SkeletonRows rows={3} />
      ) : projects.length === 0 ? (
        <div className="panel">
          <EmptyState
            icon={<FolderTree size={22} />}
            title="No projects yet"
            hint="A project scopes services, members, registries, and shared environment."
            action={
              <button type="button" className="btn btn-primary" onClick={openModal}>
                <Plus size={14} aria-hidden="true" /> New project
              </button>
            }
          />
        </div>
      ) : (
        <div className="panel overflow-hidden">
          <table className="dtable">
            <thead>
              <tr>
                <th>name</th>
                <th>description</th>
                <th className="num">env vars</th>
              </tr>
            </thead>
            <tbody>
              {projects.map((p) => (
                <tr key={p.id}>
                  <td>
                    <Link
                      to="/projects/$projectId"
                      params={{ projectId: p.id }}
                      onClick={() => setActiveProject(p.id)}
                    >
                      {p.name}
                    </Link>
                  </td>
                  <td className="text-faint">{p.description ?? '—'}</td>
                  <td className="num">
                    <Num>{p.shared_env.length}</Num>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <Modal
        open={open}
        onClose={() => setOpen(false)}
        title="New project"
        footer={
          <>
            <button type="button" className="btn" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              type="submit"
              form="create-project-form"
              className="btn btn-primary"
              disabled={create.isPending}
            >
              {create.isPending ? (
                <>
                  <span className="spin" aria-hidden="true" /> Creating
                </>
              ) : (
                'Create project'
              )}
            </button>
          </>
        }
      >
        <form id="create-project-form" onSubmit={handleCreate} className="stack">
          <div className="form-grid">
            <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
              <label className="kicker req" htmlFor="new-project-name">
                name
              </label>
              <input
                id="new-project-name"
                placeholder="payments"
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-describedby={createError ? 'new-project-error' : undefined}
                className="field-input w-full"
                required
                autoFocus
              />
            </div>
            <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
              <label className="kicker" htmlFor="new-project-description">
                description
              </label>
              <input
                id="new-project-description"
                placeholder="optional"
                type="text"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                className="field-input w-full"
              />
            </div>
          </div>

          {/* Shared env: dynamic key/value rows */}
          <div className="form-section">
            <div className="form-section-head">
              <p className="kicker m-0">shared environment</p>
            </div>
            <p className="field-help" style={{ marginTop: 0 }}>
              Injected into every service in this project. Set once at creation.
            </p>
            {sharedEnv.length > 0 ? (
              <div className="stack" style={{ gap: '0.5rem' }}>
                {sharedEnv.map((row, i) => (
                  <div className="kv-row" key={i}>
                    <input
                      type="text"
                      aria-label={`shared env key ${i + 1}`}
                      placeholder="KEY"
                      value={row.key}
                      onChange={(e) => updateEnvRow(i, { key: e.target.value })}
                      className="field-input w-full"
                    />
                    <input
                      type="text"
                      aria-label={`shared env value ${i + 1}`}
                      placeholder="value"
                      value={row.value}
                      onChange={(e) => updateEnvRow(i, { value: e.target.value })}
                      className="field-input w-full"
                    />
                    <button
                      type="button"
                      className="btn btn-icon"
                      aria-label={`remove shared env row ${i + 1}`}
                      onClick={() => removeEnvRow(i)}
                    >
                      <Trash2 size={14} aria-hidden="true" />
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
            <div className="cluster">
              <button type="button" className="btn" onClick={addEnvRow}>
                <Plus size={14} aria-hidden="true" /> add variable
              </button>
            </div>
          </div>

          {/* Default resource limits: both or neither */}
          <div className="form-section">
            <div className="form-section-head">
              <p className="kicker m-0">default resource limits</p>
            </div>
            <p className="field-help" style={{ marginTop: 0 }}>
              Optional. Leave both blank for no project-wide default.
            </p>
            <div className="form-grid">
              <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
                <label className="kicker" htmlFor="new-project-cpu">
                  cpu millis
                </label>
                <input
                  id="new-project-cpu"
                  type="number"
                  min="0"
                  inputMode="numeric"
                  placeholder="500"
                  value={cpuMillis}
                  onChange={(e) => setCpuMillis(e.target.value)}
                  className="field-input w-full tnum"
                />
                <p className="field-help">1000 = one full core.</p>
              </div>
              <div className="col-span-12 sm:col-span-6 flex flex-col gap-1">
                <label className="kicker" htmlFor="new-project-memory">
                  memory bytes
                </label>
                <input
                  id="new-project-memory"
                  type="number"
                  min="0"
                  inputMode="numeric"
                  placeholder="536870912"
                  value={memoryBytes}
                  onChange={(e) => setMemoryBytes(e.target.value)}
                  className="field-input w-full tnum"
                  aria-describedby="new-project-memory-help"
                />
                <p id="new-project-memory-help" className="field-help">
                  {memPretty ? `= ${memPretty}` : '536870912 = 512 MiB.'}
                </p>
              </div>
            </div>
          </div>

          {createError ? (
            <span id="new-project-error">
              <InlineError message={createError} />
            </span>
          ) : null}
        </form>
      </Modal>
    </main>
  )
}
