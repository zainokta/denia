import { useState } from 'react'
import type { Service, ServiceInput } from '#/effect/schema'

type ServiceSourceValue = Service['source']
type SourceType = ServiceSourceValue['type']

interface ServiceFormProps {
  projects: ReadonlyArray<{ id: string; name: string }>
  initial?: Service
  submitLabel?: string
  pending?: boolean
  error?: string
  onSubmit: (value: ServiceInput | Service) => void
}

interface EnvRow {
  id: string
  key: string
  value: string
}

const inputClass =
  'border border-[var(--border)] bg-transparent px-2 py-1 text-sm font-mono text-[var(--fg)]'

function envFromInitial(env: ReadonlyArray<readonly [string, string]>): EnvRow[] {
  return env.map(([key, value]) => ({ id: crypto.randomUUID(), key, value }))
}

function parseDomains(raw: string): string[] {
  return raw
    .split(/[\s,]+/)
    .map((d) => d.trim())
    .filter((d) => d.length > 0)
}

export function ServiceForm({
  projects,
  initial,
  submitLabel = 'create service',
  pending = false,
  error,
  onSubmit,
}: ServiceFormProps) {
  const isEdit = initial !== undefined

  const initialSource = initial?.source
  const initialType: SourceType = initialSource?.type ?? 'external_image'

  const [projectId, setProjectId] = useState(
    initial?.project_id ?? projects[0]?.id ?? '',
  )
  const [name, setName] = useState(initial?.name ?? '')
  const [domains, setDomains] = useState(initial?.domains.join(', ') ?? '')
  const [internalPort, setInternalPort] = useState(
    initial ? String(initial.internal_port) : '',
  )
  const [healthPath, setHealthPath] = useState(initial?.health_check.path ?? '/')
  const [healthTimeout, setHealthTimeout] = useState(
    initial ? String(initial.health_check.timeout_seconds) : '5',
  )
  const [tlsEnabled, setTlsEnabled] = useState(initial?.tls_enabled ?? false)

  const [cpuMillis, setCpuMillis] = useState(
    initial?.resource_limits ? String(initial.resource_limits.cpu_millis) : '',
  )
  const [memoryBytes, setMemoryBytes] = useState(
    initial?.resource_limits ? String(initial.resource_limits.memory_bytes) : '',
  )

  const [envRows, setEnvRows] = useState<EnvRow[]>(
    initial ? envFromInitial(initial.env) : [],
  )

  const [sourceType, setSourceType] =
    useState<SourceType>(initialType)

  // git source state
  const gitInitial = initialSource?.type === 'git' ? initialSource : undefined
  const [gitRepoUrl, setGitRepoUrl] = useState(gitInitial?.repo_url ?? '')
  const [gitRef, setGitRef] = useState(gitInitial?.git_ref ?? 'main')
  const [gitDockerfilePath, setGitDockerfilePath] = useState(
    gitInitial?.dockerfile_path ?? 'Dockerfile',
  )
  const [gitContextPath, setGitContextPath] = useState(
    gitInitial?.context_path ?? '.',
  )
  const [gitCredName, setGitCredName] = useState(gitInitial?.credential.name ?? '')
  const [gitCredKey, setGitCredKey] = useState(gitInitial?.credential.key ?? '')

  // external_image source state
  const extInitial =
    initialSource?.type === 'external_image' ? initialSource : undefined
  const [extImage, setExtImage] = useState(extInitial?.image ?? '')
  const [extRegistryId, setExtRegistryId] = useState(
    extInitial?.registry_id ?? '',
  )
  const [extImageRef, setExtImageRef] = useState(extInitial?.image_ref ?? '')

  const buildSource = (): ServiceSourceValue | undefined => {
    if (sourceType === 'git') {
      const repoUrl = gitRepoUrl.trim()
      if (repoUrl.length === 0) return undefined
      return {
        type: 'git',
        repo_url: repoUrl,
        git_ref: gitRef.trim() || 'main',
        dockerfile_path: gitDockerfilePath.trim() || 'Dockerfile',
        context_path: gitContextPath.trim() || '.',
        credential: { name: gitCredName.trim(), key: gitCredKey.trim() },
      }
    }
    // external_image — XOR: legacy image OR (registry_id + image_ref)
    const hasLegacy = extImage.trim().length > 0
    const hasNew =
      extRegistryId.trim().length > 0 && extImageRef.trim().length > 0
    if (hasLegacy === hasNew) return undefined // neither, or both -> invalid
    return {
      type: 'external_image',
      image: hasLegacy ? extImage.trim() : '',
      credential: null,
      registry_id: hasNew ? extRegistryId.trim() : null,
      image_ref: hasNew ? extImageRef.trim() : null,
    }
  }

  const parsedDomains = parseDomains(domains)
  const port = Number.parseInt(internalPort, 10)
  const portValid = Number.isInteger(port) && port > 0
  const source = buildSource()

  const valid =
    name.trim().length > 0 &&
    parsedDomains.length > 0 &&
    portValid &&
    source !== undefined &&
    projectId.length > 0

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!valid || source === undefined) return

    const timeout = Number.parseInt(healthTimeout, 10)
    const cpu = cpuMillis.trim()
    const mem = memoryBytes.trim()
    const hasLimits = cpu.length > 0 || mem.length > 0

    const base: ServiceInput = {
      project_id: projectId,
      name: name.trim(),
      domains: parsedDomains,
      source,
      internal_port: port,
      health_check: {
        path: healthPath.trim() || '/',
        timeout_seconds:
          Number.isInteger(timeout) && timeout > 0 ? timeout : 5,
      },
      resource_limits: hasLimits
        ? {
            cpu_millis: Number.parseInt(cpu, 10) || 0,
            memory_bytes: Number.parseInt(mem, 10) || 0,
          }
        : null,
      env: envRows
        .filter((row) => row.key.trim().length > 0)
        .map((row) => [row.key.trim(), row.value] as [string, string]),
      tls_enabled: tlsEnabled,
    }

    if (isEdit && initial) {
      onSubmit({ ...base, id: initial.id })
    } else {
      onSubmit(base)
    }
  }

  const updateEnvRow = (index: number, patch: Partial<EnvRow>) => {
    setEnvRows((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    )
  }

  return (
    <form className="panel p-4" onSubmit={handleSubmit}>
      {error ? (
        <div className="mb-3 text-xs text-[var(--violet)]">
          <span className="signal signal-fault mr-2 inline-block align-middle" />
          {error}
        </div>
      ) : null}

      <div className="mb-3 flex flex-col gap-1">
        <label className="kicker" htmlFor="sf-project">
          project
        </label>
        <select
          id="sf-project"
          className={inputClass}
          value={projectId}
          disabled={isEdit}
          onChange={(e) => setProjectId(e.target.value)}
        >
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </div>

      <div className="mb-3 flex flex-col gap-1">
        <label className="kicker" htmlFor="sf-name">
          name
        </label>
        <input
          id="sf-name"
          type="text"
          aria-label="service name"
          className={inputClass}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>

      <div className="mb-3 flex flex-col gap-1">
        <label className="kicker" htmlFor="sf-domains">
          domains
        </label>
        <input
          id="sf-domains"
          type="text"
          aria-label="domains"
          placeholder="comma or space separated"
          className={inputClass}
          value={domains}
          onChange={(e) => setDomains(e.target.value)}
        />
      </div>

      <div className="mb-3 flex flex-col gap-1">
        <label className="kicker" htmlFor="sf-port">
          internal port
        </label>
        <input
          id="sf-port"
          type="number"
          aria-label="internal port"
          className={inputClass}
          value={internalPort}
          onChange={(e) => setInternalPort(e.target.value)}
        />
      </div>

      <fieldset className="mb-3 flex flex-wrap items-center gap-4 text-sm">
        <legend className="kicker mb-1">source type</legend>
        <label className="inline-flex items-center gap-1.5 text-[var(--fg)]">
          <input
            type="radio"
            aria-label="source type git"
            name="sf-sourceType"
            value="git"
            checked={sourceType === 'git'}
            onChange={() => setSourceType('git')}
          />
          Git
        </label>
        <label className="inline-flex items-center gap-1.5 text-[var(--fg)]">
          <input
            type="radio"
            aria-label="source type external image"
            name="sf-sourceType"
            value="external_image"
            checked={sourceType === 'external_image'}
            onChange={() => setSourceType('external_image')}
          />
          External Image
        </label>
      </fieldset>

      {sourceType === 'git' ? (
        <div className="mb-3 flex flex-wrap items-end gap-2">
          <input
            type="text"
            aria-label="repo url"
            placeholder="repo url"
            value={gitRepoUrl}
            onChange={(e) => setGitRepoUrl(e.target.value)}
            className={`${inputClass} w-72`}
          />
          <input
            type="text"
            aria-label="git ref"
            placeholder="branch/tag"
            value={gitRef}
            onChange={(e) => setGitRef(e.target.value)}
            className={inputClass}
          />
          <input
            type="text"
            aria-label="dockerfile path"
            placeholder="Dockerfile path"
            value={gitDockerfilePath}
            onChange={(e) => setGitDockerfilePath(e.target.value)}
            className={inputClass}
          />
          <input
            type="text"
            aria-label="context path"
            placeholder="context path"
            value={gitContextPath}
            onChange={(e) => setGitContextPath(e.target.value)}
            className={inputClass}
          />
          <input
            type="text"
            aria-label="credential name"
            placeholder="credential name"
            value={gitCredName}
            onChange={(e) => setGitCredName(e.target.value)}
            className={inputClass}
          />
          <input
            type="text"
            aria-label="credential key"
            placeholder="credential key"
            value={gitCredKey}
            onChange={(e) => setGitCredKey(e.target.value)}
            className={inputClass}
          />
        </div>
      ) : (
        <div className="mb-3 flex flex-wrap items-end gap-2">
          <input
            type="text"
            aria-label="image"
            placeholder="image (legacy)"
            value={extImage}
            onChange={(e) => setExtImage(e.target.value)}
            className={`${inputClass} w-72`}
          />
          <span className="text-xs text-[var(--fg-muted)]">or</span>
          <input
            type="text"
            aria-label="registry id"
            placeholder="registry id"
            value={extRegistryId}
            onChange={(e) => setExtRegistryId(e.target.value)}
            className={inputClass}
          />
          <input
            type="text"
            aria-label="image ref"
            placeholder="image ref"
            value={extImageRef}
            onChange={(e) => setExtImageRef(e.target.value)}
            className={inputClass}
          />
        </div>
      )}

      <div className="mb-3 flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1">
          <label className="kicker" htmlFor="sf-health-path">
            health path
          </label>
          <input
            id="sf-health-path"
            type="text"
            aria-label="health path"
            className={inputClass}
            value={healthPath}
            onChange={(e) => setHealthPath(e.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1">
          <label className="kicker" htmlFor="sf-health-timeout">
            health timeout (s)
          </label>
          <input
            id="sf-health-timeout"
            type="number"
            aria-label="health timeout in seconds"
            className={inputClass}
            value={healthTimeout}
            onChange={(e) => setHealthTimeout(e.target.value)}
          />
        </div>
      </div>

      <div className="mb-3 flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1">
          <label className="kicker" htmlFor="sf-cpu">
            cpu millis (optional)
          </label>
          <input
            id="sf-cpu"
            type="number"
            aria-label="cpu millis"
            className={inputClass}
            value={cpuMillis}
            onChange={(e) => setCpuMillis(e.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1">
          <label className="kicker" htmlFor="sf-mem">
            memory bytes (optional)
          </label>
          <input
            id="sf-mem"
            type="number"
            aria-label="memory bytes"
            className={inputClass}
            value={memoryBytes}
            onChange={(e) => setMemoryBytes(e.target.value)}
          />
        </div>
      </div>

      <div className="mb-3">
        <p className="kicker mb-1">env</p>
        {envRows.map((row, i) => (
          <div key={row.id} className="mb-2 flex items-center gap-2">
            <input
              type="text"
              aria-label={`env key ${i}`}
              placeholder="KEY"
              value={row.key}
              onChange={(e) => updateEnvRow(i, { key: e.target.value })}
              className={inputClass}
            />
            <input
              type="text"
              aria-label={`env value ${i}`}
              placeholder="value"
              value={row.value}
              onChange={(e) => updateEnvRow(i, { value: e.target.value })}
              className={inputClass}
            />
            <button
              type="button"
              className="btn text-xs"
              onClick={() =>
                setEnvRows((rows) => rows.filter((_, idx) => idx !== i))
              }
            >
              remove
            </button>
          </div>
        ))}
        <button
          type="button"
          className="btn text-xs"
          onClick={() =>
            setEnvRows((rows) => [
              ...rows,
              { id: crypto.randomUUID(), key: '', value: '' },
            ])
          }
        >
          add env var
        </button>
      </div>

      <label className="mb-4 inline-flex items-center gap-1.5 text-sm text-[var(--fg)]">
        <input
          type="checkbox"
          aria-label="TLS enabled"
          checked={tlsEnabled}
          onChange={(e) => setTlsEnabled(e.target.checked)}
        />
        TLS enabled
      </label>

      <div>
        <button
          type="submit"
          className="btn btn-primary text-xs"
          disabled={!valid || pending}
        >
          {pending ? 'saving...' : submitLabel}
        </button>
      </div>
    </form>
  )
}
