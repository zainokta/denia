// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { render, screen } from '@testing-library/react'
import { RepositoriesTable, GcButton, PushCommandsModal } from './hosted-registry'
import type { HostedRepository } from '#/effect/schema'

describe('RepositoriesTable', () => {
  it('shows empty state when no repositories are given', () => {
    render(<RepositoriesTable repositories={[]} />)
    expect(screen.getByText('No repositories yet')).toBeTruthy()
  })

  it('renders project, service, repository and tag info for one entry', () => {
    const repo: HostedRepository = {
      project_id: 'proj-1',
      project_name: 'default',
      service_id: 'svc-1',
      service_name: 'api',
      repository: 'default/api',
      tags: [
        {
          tag: 'latest',
          digest: 'sha256:abcdef123456789012345678901234567890123456789012345678901234',
          size: 1234,
          updated_at: new Date().toISOString(),
        },
      ],
    }
    render(<RepositoriesTable repositories={[repo]} />)
    expect(screen.getByText('default/api')).toBeTruthy()
    expect(screen.getByText('default')).toBeTruthy()
    expect(screen.getByText('api')).toBeTruthy()
    expect(screen.getByText('latest')).toBeTruthy()
    // formatBytes(1234) → "1.2 KiB"
    expect(screen.getByText('1.2 KiB')).toBeTruthy()
  })
})

describe('GcButton', () => {
  it('button is disabled when busy is true', () => {
    const { container } = render(<GcButton busy={true} onConfirm={() => {}} />)
    const btn = container.querySelector('button')
    expect(btn?.hasAttribute('disabled')).toBe(true)
  })

  it('button is not disabled when busy is false', () => {
    const { container } = render(<GcButton busy={false} onConfirm={() => {}} />)
    const btn = container.querySelector('button')
    expect(btn?.hasAttribute('disabled')).toBe(false)
  })
})

describe('PushCommandsModal', () => {
  it('renders push command containing the repository name when open', () => {
    render(<PushCommandsModal repository="default/api" open={true} onClose={() => {}} />)
    // The push command line must be visible and include the repository path.
    const el = screen.getByText(/docker push/)
    expect(el).toBeTruthy()
    expect(el.textContent).toContain('default/api')
  })

  it('renders nothing when open is false', () => {
    const { container } = render(
      <PushCommandsModal repository="default/api" open={false} onClose={() => {}} />,
    )
    // Modal returns null when closed — container should have no push command text.
    expect(container.querySelector('code')).toBeNull()
  })
})
