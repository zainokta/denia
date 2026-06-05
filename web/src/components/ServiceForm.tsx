import { useEffect, useRef, useState } from 'react'
import { ChevronDown, ChevronUp } from 'lucide-react'
import type { Service, ServiceEndpoint, ServiceInput } from '#/effect/schema'
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

type EndpointProtocol = ServiceEndpoint['protocol']

interface EndpointRow {
  id: string
  name: string
  protocol: EndpointProtocol
  internalPort: string
}

// Mirrors backend `validate_endpoint_name` (src/domain/service.rs): ASCII
// alphanumeric plus '-' and '_'. Validated client-side so the operator sees the
// problem inline instead of a 400 on submit.
const ENDPOINT_NAME_RE = /^[A-Za-z0-9_-]+$/

const inputClass = 'field-input'

function envFromInitial(env: ReadonlyArray<readonly [string, string]>): EnvRow[] {
  return env.map(([key, value]) => ({ id: crypto.randomUUID(), key, value }))
}

function endpointsFromInitial(
  endpoints: ReadonlyArray<ServiceEndpoint>,
): EndpointRow[] {
  return endpoints.map((e) => ({
    id: crypto.randomUUID(),
    name: e.name,
    protocol: e.protocol,
    internalPort: String(e.internal_port),
  }))
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

  // Autoscale policy (mirrors backend AutoscalePolicy). Enabled flag toggles the
  // whole block; numeric fields are seeded from the existing policy or defaults.
  const initialAutoscale = initial?.autoscale ?? undefined
  const [autoscaleEnabled, setAutoscaleEnabled] = useState(
    initialAutoscale != null,
  )
  const [minReplicas, setMinReplicas] = useState(
    initialAutoscale ? String(initialAutoscale.min_replicas) : '1',
  )
  const [maxReplicas, setMaxReplicas] = useState(
    initialAutoscale ? String(initialAutoscale.max_replicas) : '3',
  )
  const [targetCpuPct, setTargetCpuPct] = useState(
    initialAutoscale ? String(initialAutoscale.target_cpu_pct) : '70',
  )
  const [targetMemPct, setTargetMemPct] = useState(
    initialAutoscale?.target_mem_pct != null
      ? String(initialAutoscale.target_mem_pct)
      : '',
  )
  const [cooldownS, setCooldownS] = useState(
    initialAutoscale ? String(initialAutoscale.scale_down_cooldown_s) : '300',
  )
  const [idleTimeoutS, setIdleTimeoutS] = useState(
    initialAutoscale ? String(initialAutoscale.idle_timeout_s) : '600',
  )

  const [envRows, setEnvRows] = useState<EnvRow[]>(
    initial ? envFromInitial(initial.env) : [],
  )

  const [endpointRows, setEndpointRows] = useState<EndpointRow[]>(
    initial?.endpoints ? endpointsFromInitial(initial.endpoints) : [],
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
      ((initial?.env.length ?? 0) > 0 ||
        initial?.resource_limits != null ||
        initial?.autoscale != null),
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
    if (sourceType === 'upload') {
      // Upload-deployed service (ADR-039): no source config — the build context
      // is supplied per-deploy by `denia push`. Always valid.
      return { type: 'upload' }
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

  // Autoscale validation mirrors backend AutoscalePolicy::validate so the
  // operator sees the problem inline instead of a 400 on submit.
  const autoscaleError: string | null = (() => {
    if (!autoscaleEnabled) return null
    const min = Number.parseInt(minReplicas, 10)
    const max = Number.parseInt(maxReplicas, 10)
    const cpu = Number.parseInt(targetCpuPct, 10)
    const cooldown = Number.parseInt(cooldownS, 10)
    const idle = Number.parseInt(idleTimeoutS, 10)
    const memRaw = targetMemPct.trim()
    const mem = memRaw.length > 0 ? Number.parseInt(memRaw, 10) : null
    const pctOk = (p: number) => Number.isInteger(p) && p >= 1 && p <= 100
    if (
      !Number.isInteger(min) ||
      !Number.isInteger(max) ||
      min < 0 ||
      max < 1 ||
      min > max
    )
      return 'replica bounds: need max ≥ 1 and 0 ≤ min ≤ max'
    if (!pctOk(cpu)) return 'target cpu must be 1–100%'
    if (mem !== null && !pctOk(mem)) return 'target memory must be 1–100%'
    if (!Number.isInteger(cooldown) || cooldown < 0)
      return 'cooldown must be a non-negative integer'
    if (!Number.isInteger(idle) || idle < cooldown)
      return 'idle timeout must be ≥ cooldown'
    return null
  })()

  // Endpoint validation mirrors backend `ServiceEndpoint::validate`: a non-empty
  // safe name and an internal port in 1–65535. Public ports are auto-allocated
  // (ADR-036), so the form never collects them.
  const endpointsError: string | null = (() => {
    for (const row of endpointRows) {
      const name = row.name.trim()
      if (name.length === 0) return 'endpoint name is required'
      if (!ENDPOINT_NAME_RE.test(name))
        return `endpoint "${name}": only letters, digits, - and _ allowed`
      const port = Number.parseInt(row.internalPort, 10)
      if (!Number.isInteger(port) || port < 1 || port > 65535)
        return `endpoint "${name}": internal port must be 1–65535`
    }
    return null
  })()

  const valid =
    !nameEmpty &&
    portValid &&
    source !== undefined &&
    projectId.length > 0 &&
    autoscaleError === null &&
    endpointsError === null

  const missing: string[] = []
  if (projectId.length === 0) missing.push('project')
  if (nameEmpty) missing.push('name')
  if (!portValid) missing.push('valid port')
  if (source === undefined) {
    if (sourceType === 'git') missing.push('repo url')
    else if (imageMode === 'direct') missing.push('image')
    else missing.push('registry + ref')
  }
  if (autoscaleError !== null) missing.push('autoscale config')
  if (endpointsError !== null) missing.push('endpoint config')

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
      endpoints: endpointRows
        .filter((row) => row.name.trim().length > 0)
        .map((row) => ({
          name: row.name.trim(),
          protocol: row.protocol,
          internal_port: Number.parseInt(row.internalPort, 10) || 0,
          // Public ports are Denia-allocated server-side (ADR-036); http never
          // carries one. The form always sends null.
          public_port: null,
        })),
      autoscale: autoscaleEnabled
        ? {
            min_replicas: Number.parseInt(minReplicas, 10) || 0,
            max_replicas: Number.parseInt(maxReplicas, 10) || 0,
            target_cpu_pct: Number.parseInt(targetCpuPct, 10) || 0,
            target_mem_pct: targetMemPct.trim()
              ? Number.parseInt(targetMemPct, 10) || 0
              : null,
            scale_down_cooldown_s: Number.parseInt(cooldownS, 10) || 0,
            idle_timeout_s: Number.parseInt(idleTimeoutS, 10) || 0,
          }
        : null,
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

  const updateEndpointRow = (index: number, patch: Partial<EndpointRow>) => {
    setEndpointRows((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    )
  }

  // Narrow the native select value to the protocol union without a cast, keeping
  // with the form's cast-free convention.
  const toProtocol = (value: string): EndpointProtocol =>
    value === 'http' ? 'http' : value === 'udp' ? 'udp' : 'tcp'

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

      <div className="mb-5">
        <div className="form-section-head" style={{ marginBottom: '0.4rem' }}>
          <p className="kicker">protocol endpoints</p>
          <span className="text-xs text-[var(--fg-muted)]">(optional)</span>
        </div>
        <p
          className="field-help"
          style={{ marginTop: 0, marginBottom: '0.75rem' }}
          id="sf-endpoints-help"
        >
          The internal port above is served as HTTP and routed by domain. Add
          TCP/UDP endpoints for game servers or raw protocols — public ports are
          auto-allocated by Denia. TCP/UDP services are always-on (no
          scale-to-zero).
        </p>

        {endpointRows.map((row, i) => (
          <div key={row.id} className="mb-2 flex flex-wrap items-center gap-2">
            <input
              type="text"
              aria-label={`endpoint name ${i}`}
              placeholder="name"
              value={row.name}
              onChange={(e) => updateEndpointRow(i, { name: e.target.value })}
              className={`${inputClass} w-32`}
            />
            <select
              aria-label={`endpoint protocol ${i}`}
              value={row.protocol}
              onChange={(e) =>
                updateEndpointRow(i, { protocol: toProtocol(e.target.value) })
              }
              className={`${inputClass} w-24`}
            >
              <option value="http">http</option>
              <option value="tcp">tcp</option>
              <option value="udp">udp</option>
            </select>
            <span className="field-input-group">
              <input
                type="number"
                inputMode="numeric"
                min={1}
                max={65535}
                aria-label={`endpoint internal port ${i}`}
                placeholder="port"
                value={row.internalPort}
                onChange={(e) =>
                  updateEndpointRow(i, { internalPort: e.target.value })
                }
                className={`${inputClass} w-24 tnum`}
              />
              <span className="field-suffix">internal</span>
            </span>
            <span
              className="badge"
              title="public port is auto-allocated by Denia"
            >
              {row.protocol === 'http' ? 'via domain' : 'public: auto'}
            </span>
            <button
              type="button"
              className="btn text-xs"
              aria-label={`remove endpoint ${i}`}
              onClick={() =>
                setEndpointRows((rows) => rows.filter((_, idx) => idx !== i))
              }
            >
              remove
            </button>
          </div>
        ))}

        <button
          type="button"
          className="btn text-xs"
          aria-describedby="sf-endpoints-help"
          onClick={() =>
            setEndpointRows((rows) => [
              ...rows,
              {
                id: crypto.randomUUID(),
                name: '',
                protocol: 'tcp',
                internalPort: '',
              },
            ])
          }
        >
          add endpoint
        </button>

        {endpointsError ? (
          <p
            className="field-error"
            role="alert"
            style={{ marginTop: '0.6rem' }}
          >
            {endpointsError}
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
        <label className="inline-flex items-center gap-2 text-[var(--fg)]">
          <input
            type="radio"
            className="field-check"
            aria-label="source type upload"
            name="sf-sourceType"
            value="upload"
            checked={sourceType === 'upload'}
            onChange={() => setSourceType('upload')}
          />
          Upload (deploy via CLI push)
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
                    Optional. For a private repo, the name of an SSH deploy key
                    credential (a SOPS-encrypted key placed under the project
                    secrets directory out of band). Leave blank for public
                    repos.
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
      ) : sourceType === 'upload' ? (
        <div className="mb-5">
          <p className="field-help" style={{ marginTop: 0 }}>
            Deploy this service's image from your machine with{' '}
            <code>denia push</code>. No source configuration is needed here —
            run <code>denia init</code>, then <code>denia create</code> and{' '}
            <code>denia push</code>.
          </p>
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

            <div className="mb-4">
              <label className="mb-2 inline-flex items-center gap-2 text-sm text-[var(--fg)]">
                <input
                  type="checkbox"
                  className="field-check"
                  aria-label="enable autoscaling"
                  checked={autoscaleEnabled}
                  onChange={(e) => setAutoscaleEnabled(e.target.checked)}
                />
                enable autoscaling
              </label>
              {autoscaleEnabled ? (
                <>
                  <div className="mb-3 flex flex-wrap items-end gap-2">
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-min">
                        min replicas
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-min"
                          type="number"
                          min={0}
                          aria-label="min replicas"
                          className={`${inputClass} w-24 tnum`}
                          value={minReplicas}
                          onChange={(e) => setMinReplicas(e.target.value)}
                        />
                        <span className="field-suffix">replicas</span>
                      </span>
                    </div>
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-max">
                        max replicas
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-max"
                          type="number"
                          min={1}
                          aria-label="max replicas"
                          className={`${inputClass} w-24 tnum`}
                          value={maxReplicas}
                          onChange={(e) => setMaxReplicas(e.target.value)}
                        />
                        <span className="field-suffix">replicas</span>
                      </span>
                    </div>
                  </div>

                  <div className="mb-3 flex flex-wrap items-end gap-2">
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-cpu">
                        target cpu
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-cpu"
                          type="number"
                          min={1}
                          max={100}
                          aria-label="target cpu percent"
                          className={`${inputClass} w-24 tnum`}
                          value={targetCpuPct}
                          onChange={(e) => setTargetCpuPct(e.target.value)}
                        />
                        <span className="field-suffix">%</span>
                      </span>
                    </div>
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-mem">
                        target memory (optional)
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-mem"
                          type="number"
                          min={1}
                          max={100}
                          aria-label="target memory percent"
                          className={`${inputClass} w-24 tnum`}
                          value={targetMemPct}
                          onChange={(e) => setTargetMemPct(e.target.value)}
                        />
                        <span className="field-suffix">%</span>
                      </span>
                    </div>
                  </div>

                  <div className="mb-2 flex flex-wrap items-end gap-2">
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-cooldown">
                        scale-down cooldown
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-cooldown"
                          type="number"
                          min={0}
                          aria-label="scale down cooldown seconds"
                          className={`${inputClass} w-28 tnum`}
                          value={cooldownS}
                          onChange={(e) => setCooldownS(e.target.value)}
                        />
                        <span className="field-suffix">s</span>
                      </span>
                    </div>
                    <div className="flex flex-col gap-1">
                      <label className="kicker" htmlFor="sf-as-idle">
                        idle timeout
                      </label>
                      <span className="field-input-group">
                        <input
                          id="sf-as-idle"
                          type="number"
                          min={0}
                          aria-label="idle timeout seconds"
                          className={`${inputClass} w-28 tnum`}
                          value={idleTimeoutS}
                          onChange={(e) => setIdleTimeoutS(e.target.value)}
                        />
                        <span className="field-suffix">s</span>
                      </span>
                    </div>
                  </div>

                  <p className="field-help">
                    min 0 = scale to zero when idle; first request cold-starts a
                    replica
                  </p>
                  {autoscaleError ? (
                    <p className="field-error" role="alert">
                      {autoscaleError}
                    </p>
                  ) : null}
                </>
              ) : null}
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
