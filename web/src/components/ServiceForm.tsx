import { useEffect, useRef, useState } from 'react'
import { ChevronDown, ChevronUp } from 'lucide-react'
import type { Service, ServiceInput } from '#/effect/schema'
import { DomainTagInput } from '#/components/DomainTagInput'
import { FieldHint } from '#/components/FieldHint'

type ServiceSourceValue = Service['source']
type SourceType = ServiceSourceValue['type']
type ImageMode = 'direct' | 'registry'
type RequiredField = 'name' | 'port'

interface RegistryOption {
  id: string
  name: string
  project_id: string
  endpoint: string
}

interface ServiceFormProps {
  projects: ReadonlyArray<{ id: string; name: string }>
  registries?: ReadonlyArray<RegistryOption>
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

const inputClass = 'field-input'

function envFromInitial(env: ReadonlyArray<readonly [string, string]>): EnvRow[] {
  return env.map(([key, value]) => ({ id: crypto.randomUUID(), key, value }))
}

export function ServiceForm({
  projects,
  registries = [],
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
  const [domains, setDomains] = useState<string[]>(
    initial?.domains ? [...initial.domains] : [],
  )
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

  const [sourceType, setSourceType] = useState<SourceType>(initialType)

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
  // Explicit mode replaces inferring direct-vs-registry from which fields are
  // filled, which let "both filled" fail silently.
  const [imageMode, setImageMode] = useState<ImageMode>(
    extInitial?.registry_id ? 'registry' : 'direct',
  )

  // Advanced block is collapsed for the common path; opened on edit when the
  // service already carries non-default health/resource/env values.
  const [advancedOpen, setAdvancedOpen] = useState(
    isEdit &&
      ((initial?.env.length ?? 0) > 0 || initial?.resource_limits != null),
  )

  // A required field flags its error only after it has been blurred, so the
  // form stays quiet while the operator is still typing.
  const [touched, setTouched] = useState<Record<RequiredField, boolean>>({
    name: false,
    port: false,
  })
  const markTouched = (field: RequiredField) =>
    setTouched((t) => ({ ...t, [field]: true }))

  const nameRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    nameRef.current?.focus()
  }, [])

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
    if (imageMode === 'direct') {
      const image = extImage.trim()
      if (image.length === 0) return undefined
      return {
        type: 'external_image',
        image,
        credential: null,
        registry_id: null,
        image_ref: null,
      }
    }
    const registryId = extRegistryId.trim()
    const imageRef = extImageRef.trim()
    if (registryId.length === 0 || imageRef.length === 0) return undefined
    return {
      type: 'external_image',
      image: '',
      credential: null,
      registry_id: registryId,
      image_ref: imageRef,
    }
  }

  const parsedDomains = domains
  const port = Number.parseInt(internalPort, 10)
  const portValid = Number.isInteger(port) && port > 0
  const source = buildSource()

  const nameEmpty = name.trim().length === 0

  const valid =
    !nameEmpty &&
    portValid &&
    source !== undefined &&
    projectId.length > 0

  const missing: string[] = []
  if (projectId.length === 0) missing.push('project')
  if (nameEmpty) missing.push('name')
  if (!portValid) missing.push('valid port')
  if (source === undefined) {
    if (sourceType === 'git') missing.push('repo url')
    else if (imageMode === 'direct') missing.push('image')
    else missing.push('registry + ref')
  }

  // Inline error shows only for a required field that's been blurred and is
  // still empty/invalid.
  const err = (field: RequiredField, bad: boolean) => touched[field] && bad
  const fieldClass = (field: RequiredField, bad: boolean) =>
    `${inputClass}${err(field, bad) ? ' is-invalid' : ''}`
  const labelClass = (field: RequiredField, bad: boolean) =>
    `kicker req${err(field, bad) ? ' err' : ''}`

  const handleSubmit = (e: React.SyntheticEvent<HTMLFormElement>) => {
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
      tls_enabled: parsedDomains.length > 0 && tlsEnabled,
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
    <form onSubmit={handleSubmit}>
      {error ? (
        <div role="alert" className="mb-4 text-xs text-[var(--violet)]">
          <span className="signal signal-fault mr-2 inline-block align-middle" />
          {error}
        </div>
      ) : null}

      <div className="mb-4 flex flex-col gap-1">
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

      <div className="mb-4 flex flex-col gap-1">
        <label className={labelClass('name', nameEmpty)} htmlFor="sf-name">
          name
        </label>
        <input
          id="sf-name"
          ref={nameRef}
          type="text"
          aria-required="true"
          aria-invalid={err('name', nameEmpty)}
          aria-describedby={
            err('name', nameEmpty) ? 'sf-name-error' : undefined
          }
          className={fieldClass('name', nameEmpty)}
          value={name}
          onChange={(e) => setName(e.target.value)}
          onBlur={() => markTouched('name')}
        />
        {err('name', nameEmpty) ? (
          <p id="sf-name-error" className="field-error" role="alert">
            name is required
          </p>
        ) : null}
      </div>

      <div className="mb-4 flex flex-col gap-1">
        <label className="kicker" htmlFor="sf-domains">
          domains{' '}
          <span className="text-xs text-[var(--fg-muted)]">(optional)</span>
        </label>
        <DomainTagInput
          id="sf-domains"
          value={domains}
          onChange={setDomains}
          ariaDescribedBy="sf-domains-help"
        />
        <p id="sf-domains-help" className="field-help">
          press space, comma, or enter to add
        </p>
      </div>

      <div className="mb-5 flex flex-col gap-1">
        <label className={labelClass('port', !portValid)} htmlFor="sf-port">
          internal port
        </label>
        <input
          id="sf-port"
          type="number"
          inputMode="numeric"
          min={1}
          aria-required="true"
          aria-invalid={err('port', !portValid)}
          aria-describedby={
            err('port', !portValid) ? 'sf-port-error' : undefined
          }
          className={`${fieldClass('port', !portValid)} w-32 tnum`}
          value={internalPort}
          onChange={(e) => setInternalPort(e.target.value)}
          onBlur={() => markTouched('port')}
        />
        {err('port', !portValid) ? (
          <p id="sf-port-error" className="field-error" role="alert">
            port must be a positive integer
          </p>
        ) : null}
      </div>

      <fieldset className="mb-4 flex flex-wrap items-center gap-4 text-sm">
        <legend className="kicker mb-1">source type</legend>
        <label className="inline-flex items-center gap-2 text-[var(--fg)]">
          <input
            type="radio"
            className="field-check"
            aria-label="source type git"
            name="sf-sourceType"
            value="git"
            checked={sourceType === 'git'}
            onChange={() => setSourceType('git')}
          />
          Git
        </label>
        <label className="inline-flex items-center gap-2 text-[var(--fg)]">
          <input
            type="radio"
            className="field-check"
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
        <div className="mb-5">
          <div className="form-section">
            <div className="form-grid">
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-8">
                <label className="kicker req" htmlFor="sf-git-repo">
                  repo url
                </label>
                <input
                  id="sf-git-repo"
                  type="text"
                  aria-required="true"
                  placeholder="https://github.com/org/repo"
                  value={gitRepoUrl}
                  onChange={(e) => setGitRepoUrl(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-4">
                <label className="kicker" htmlFor="sf-git-ref">
                  branch/tag
                </label>
                <input
                  id="sf-git-ref"
                  type="text"
                  placeholder="main"
                  value={gitRef}
                  onChange={(e) => setGitRef(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
            </div>
          </div>

          <div className="form-section">
            <div className="form-section-head">
              <p className="kicker">build</p>
              <span className="text-xs text-[var(--fg-muted)]">
                relative to repo root
              </span>
            </div>
            <div className="form-grid">
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                <label className="kicker" htmlFor="sf-git-dockerfile">
                  dockerfile path
                </label>
                <input
                  id="sf-git-dockerfile"
                  type="text"
                  placeholder="Dockerfile"
                  value={gitDockerfilePath}
                  onChange={(e) => setGitDockerfilePath(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                <label className="kicker" htmlFor="sf-git-context">
                  context path
                </label>
                <input
                  id="sf-git-context"
                  type="text"
                  placeholder="."
                  value={gitContextPath}
                  onChange={(e) => setGitContextPath(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
            </div>
          </div>

          <div className="form-section">
            <div className="form-section-head">
              <p className="kicker">auth</p>
              <span className="text-xs text-[var(--fg-muted)]">
                private repos only
              </span>
            </div>
            <div className="form-grid">
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                <div className="flex items-center gap-1.5">
                  <label className="kicker" htmlFor="sf-git-cred-name">
                    credential name
                  </label>
                  <FieldHint
                    id="hint-git-cred-name"
                    label="about credential name"
                  >
                    Name of the credential registered via{' '}
                    <code>POST /v1/credentials/git</code>. Points to the SOPS
                    file holding the SSH deploy key Denia uses to clone the
                    repo.
                  </FieldHint>
                </div>
                <input
                  id="sf-git-cred-name"
                  type="text"
                  placeholder="deploy-key"
                  value={gitCredName}
                  onChange={(e) => setGitCredName(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
              <div className="flex flex-col gap-1 col-span-12 sm:col-span-6">
                <div className="flex items-center gap-1.5">
                  <label className="kicker" htmlFor="sf-git-cred-key">
                    credential key
                  </label>
                  <FieldHint
                    id="hint-git-cred-key"
                    label="about credential key"
                  >
                    Field inside the SOPS payload to read. Denia stores the
                    private SSH key here; the matching public key lives on the
                    Git host as a deploy key.
                  </FieldHint>
                </div>
                <input
                  id="sf-git-cred-key"
                  type="text"
                  placeholder="ssh_key"
                  value={gitCredKey}
                  onChange={(e) => setGitCredKey(e.target.value)}
                  className={`${inputClass} w-full`}
                />
              </div>
            </div>
          </div>
        </div>
      ) : (
        <div className="mb-5">
          <p className="kicker req mb-2">image source</p>
          <div
            className="segmented mb-3"
            role="group"
            aria-label="image source mode"
          >
            <button
              type="button"
              aria-pressed={imageMode === 'direct'}
              onClick={() => setImageMode('direct')}
            >
              direct image
            </button>
            <button
              type="button"
              aria-pressed={imageMode === 'registry'}
              onClick={() => setImageMode('registry')}
            >
              registry + ref
            </button>
          </div>

          {imageMode === 'direct' ? (
            <div className="flex flex-col gap-1">
              <label className="kicker" htmlFor="sf-ext-image">
                image
              </label>
              <input
                id="sf-ext-image"
                type="text"
                placeholder="ghcr.io/org/app:latest"
                value={extImage}
                onChange={(e) => setExtImage(e.target.value)}
                className={`${inputClass} w-full sm:w-96`}
              />
            </div>
          ) : (
            <div className="flex flex-wrap items-end gap-2">
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-ext-registry">
                  registry
                </label>
                <select
                  id="sf-ext-registry"
                  value={extRegistryId}
                  onChange={(e) => setExtRegistryId(e.target.value)}
                  className={`${inputClass} w-48`}
                >
                  <option value="">select registry</option>
                  {registries
                    .filter((r) => r.project_id === projectId)
                    .map((r) => (
                      <option key={r.id} value={r.id}>
                        {r.name}
                      </option>
                    ))}
                </select>
              </div>
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-ext-ref">
                  image ref
                </label>
                <input
                  id="sf-ext-ref"
                  type="text"
                  placeholder="org/app:latest"
                  value={extImageRef}
                  onChange={(e) => setExtImageRef(e.target.value)}
                  className={`${inputClass} w-full sm:w-72`}
                />
              </div>
            </div>
          )}
        </div>
      )}

      <div className="mb-4 border-t border-[var(--border)] pt-3">
        <button
          type="button"
          className="disclosure"
          aria-expanded={advancedOpen}
          aria-controls="sf-advanced"
          onClick={() => setAdvancedOpen((v) => !v)}
        >
          <span className="kicker">advanced</span>
          {advancedOpen ? (
            <ChevronUp size={14} aria-hidden="true" />
          ) : (
            <ChevronDown size={14} aria-hidden="true" />
          )}
        </button>

        {advancedOpen ? (
          <div id="sf-advanced" className="pt-3">
            <div className="mb-4 flex flex-wrap items-end gap-2">
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-health-path">
                  health path
                </label>
                <input
                  id="sf-health-path"
                  type="text"
                  className={inputClass}
                  value={healthPath}
                  onChange={(e) => setHealthPath(e.target.value)}
                />
              </div>
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-health-timeout">
                  health timeout
                </label>
                <span className="field-input-group">
                  <input
                    id="sf-health-timeout"
                    type="number"
                    aria-label="health timeout in seconds"
                    className={`${inputClass} w-24 tnum`}
                    value={healthTimeout}
                    onChange={(e) => setHealthTimeout(e.target.value)}
                  />
                  <span className="field-suffix">s</span>
                </span>
              </div>
            </div>

            <div className="mb-4 flex flex-wrap items-end gap-2">
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-cpu">
                  cpu (optional)
                </label>
                <span className="field-input-group">
                  <input
                    id="sf-cpu"
                    type="number"
                    aria-label="cpu millis"
                    className={`${inputClass} w-28 tnum`}
                    value={cpuMillis}
                    onChange={(e) => setCpuMillis(e.target.value)}
                  />
                  <span className="field-suffix">millis</span>
                </span>
              </div>
              <div className="flex flex-col gap-1">
                <label className="kicker" htmlFor="sf-mem">
                  memory (optional)
                </label>
                <span className="field-input-group">
                  <input
                    id="sf-mem"
                    type="number"
                    aria-label="memory bytes"
                    className={`${inputClass} w-36 tnum`}
                    value={memoryBytes}
                    onChange={(e) => setMemoryBytes(e.target.value)}
                  />
                  <span className="field-suffix">bytes</span>
                </span>
              </div>
            </div>

            <div className="mb-1">
              <p className="kicker mb-2">env</p>
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
          </div>
        ) : null}
      </div>

      <div className="mb-5 flex flex-col gap-1">
        <label className="inline-flex items-center gap-2 text-sm text-[var(--fg)]">
          <input
            type="checkbox"
            className="field-check"
            aria-label="TLS enabled"
            checked={tlsEnabled && parsedDomains.length > 0}
            disabled={parsedDomains.length === 0}
            onChange={(e) => setTlsEnabled(e.target.checked)}
          />
          TLS enabled
        </label>
        {parsedDomains.length === 0 ? (
          <p className="field-help">add a domain to enable TLS</p>
        ) : null}
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <button
          type="submit"
          className="btn btn-primary text-xs"
          disabled={!valid || pending}
        >
          {pending ? 'saving...' : submitLabel}
        </button>
        {!valid && missing.length > 0 ? (
          <p className="field-help" aria-live="polite">
            needs: {missing.join(', ')}
          </p>
        ) : null}
      </div>
    </form>
  )
}
