// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { useLogStream } from './useLogStream'

vi.mock('#/effect/config', () => ({
  getApiBaseUrl: () => '',
  getApiAuthToken: () => 'test-token',
}))

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

// A ReadableStream that emits the given SSE frames then stays open until the
// signal aborts, so we can assert teardown on unmount / path change.
function sseResponse(frames: ReadonlyArray<string>, signal: AbortSignal) {
  const encoder = new TextEncoder()
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const f of frames) controller.enqueue(encoder.encode(f))
      const onAbort = () => {
        try {
          controller.close()
        } catch {
          /* already closed */
        }
      }
      if (signal.aborted) onAbort()
      else signal.addEventListener('abort', onAbort, { once: true })
    },
  })
  return new Response(body, { status: 200 })
}

describe('useLogStream', () => {
  it('parses data frames and reports streaming status', async () => {
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockImplementation((_url, init) =>
        Promise.resolve(
          sseResponse(['data: hello\n\n', 'data: world\n\n'], init!.signal!),
        ),
      )

    const { result } = renderHook(() => useLogStream('/v1/logs/stream'))

    await waitFor(() => {
      expect(result.current.lines.map((l) => l.text)).toEqual(['hello', 'world'])
    })
    expect(result.current.status).toBe('streaming')
    expect(fetchMock).toHaveBeenCalledOnce()
  })

  it('sends the bearer token from config', async () => {
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockImplementation((_url, init) =>
        Promise.resolve(sseResponse([], init!.signal!)),
      )

    renderHook(() => useLogStream('/v1/logs/stream'))

    await waitFor(() => expect(fetchMock).toHaveBeenCalled())
    const init = fetchMock.mock.calls[0][1] as RequestInit
    expect((init.headers as Record<string, string>).authorization).toBe(
      'Bearer test-token',
    )
  })

  it('aborts the in-flight request on unmount', async () => {
    let captured: AbortSignal | null = null
    vi.spyOn(globalThis, 'fetch').mockImplementation((_url, init) => {
      captured = init!.signal!
      return Promise.resolve(sseResponse([], init!.signal!))
    })

    const { unmount } = renderHook(() => useLogStream('/v1/logs/stream'))
    await waitFor(() => expect(captured).not.toBeNull())
    expect(captured!.aborted).toBe(false)

    act(() => unmount())
    expect(captured!.aborted).toBe(true)
  })

  it('tears down the previous request when the path changes', async () => {
    const signals: AbortSignal[] = []
    vi.spyOn(globalThis, 'fetch').mockImplementation((_url, init) => {
      signals.push(init!.signal!)
      return Promise.resolve(sseResponse([], init!.signal!))
    })

    const { rerender } = renderHook(({ path }) => useLogStream(path), {
      initialProps: { path: '/v1/a/stream' },
    })
    await waitFor(() => expect(signals.length).toBe(1))

    rerender({ path: '/v1/b/stream' })
    await waitFor(() => expect(signals.length).toBe(2))

    // The first stream's controller was aborted when the path changed.
    expect(signals[0].aborted).toBe(true)
    expect(signals[1].aborted).toBe(false)
  })

  it('surfaces an auth error for 401/403 responses', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 403 }),
    )

    const { result } = renderHook(() => useLogStream('/v1/logs/stream'))
    await waitFor(() => expect(result.current.status).toBe('error'))
    expect(result.current.error).toMatch(/Not authorized/)
  })
})
