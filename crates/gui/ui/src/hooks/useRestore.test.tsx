import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { RestoreEvent, RestoreRunOutcome } from '@/types/restore'

/** A minimal stand-in for `@tauri-apps/api/core`'s real `Channel`, mirroring `useRun.test.tsx`'s
 * identical fake — the mocked IPC boundary spec #97's Testing Decisions call for. */
const { FakeChannel } = vi.hoisted(() => {
  class FakeChannel<T> {
    onmessage: (event: T) => void = () => {}
  }
  return { FakeChannel }
})

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
  Channel: FakeChannel,
}))

import { invoke } from '@/lib/ipc'

import { useRestore } from './useRestore'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

interface CapturedChannel {
  onmessage: (event: RestoreEvent) => void
}

/** Wires `mockedInvoke` so `run_restore` returns a promise this test controls the resolution
 * of, capturing the `Channel` instance the hook constructed. `cancel_restore` and
 * `resolve_collision` always resolve immediately — fire-and-forget commands whose own promise
 * carries no information the hook reads. */
function mockRunRestore() {
  let resolveRun!: (outcome: RestoreRunOutcome) => void
  let capturedChannel: CapturedChannel | undefined

  mockedInvoke.mockImplementation((command: string, args?: unknown) => {
    if (command === 'run_restore') {
      capturedChannel = (args as { channel: CapturedChannel } | undefined)?.channel
      return new Promise<RestoreRunOutcome>((resolve) => {
        resolveRun = resolve
      })
    }
    if (command === 'cancel_restore' || command === 'resolve_collision') {
      return Promise.resolve(undefined)
    }
    throw new Error(`unexpected invoke: ${command}`)
  })

  return {
    resolve: (outcome: RestoreRunOutcome) => resolveRun(outcome),
    channel: () => {
      if (!capturedChannel) {
        throw new Error('run_restore was not invoked yet — no Channel captured')
      }
      return capturedChannel
    },
  }
}

const SUCCESS_OUTCOME: RestoreRunOutcome = {
  type: 'success',
  data: {
    restoredCount: 3,
    failures: [],
    skippedCollisions: [],
    ownership: 'skippedNonRoot',
    reinstallScriptPath: '/home/tester/reinstall.sh',
    packageCounts: {},
    cancelled: false,
  },
}

describe('useRestore', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('starts idle, with no outcome and no pending collision', () => {
    const { result } = renderHook(() => useRestore(), { wrapper })

    expect(result.current.status).toBe('idle')
    expect(result.current.outcome).toBeNull()
    expect(result.current.collisionRequest).toBeNull()
    expect(result.current.awaitingCollisionAnswer).toBe(false)
  })

  it('folds phase and entry progress events from the mocked channel', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'verifyingArchive' }))
    expect(result.current.progress.phase).toBe('verifyingArchive')

    act(() => run.channel().onmessage({ type: 'phase', phase: 'restoring' }))
    act(() => run.channel().onmessage({ type: 'entry', done: 4, total: 10 }))
    expect(result.current.progress.entryDone).toBe(4)
    expect(result.current.progress.entryTotal).toBe(10)

    act(() => run.resolve(SUCCESS_OUTCOME))
    await waitFor(() => expect(result.current.status).toBe('success'))
  })

  it('invokes run_restore with the picked backup dir and passphrase', async () => {
    mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x', 'hunter2'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    expect(mockedInvoke).toHaveBeenCalledWith(
      'run_restore',
      expect.objectContaining({ backupDir: '/backups/x', passphrase: 'hunter2' }),
    )
  })

  it('a plaintext restore omits the passphrase (sent as null)', async () => {
    mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    expect(mockedInvoke).toHaveBeenCalledWith(
      'run_restore',
      expect.objectContaining({ backupDir: '/backups/x', passphrase: null }),
    )
  })

  // --- the collision request→response interaction (issue #103's hard part) -----------------

  it('a collisionRequest arriving over the channel surfaces the confirmation', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    expect(result.current.awaitingCollisionAnswer).toBe(false)

    act(() =>
      run.channel().onmessage({
        type: 'collisionRequest',
        paths: ['/home/tester/.zshrc', '/home/tester/.bashrc'],
      }),
    )

    expect(result.current.awaitingCollisionAnswer).toBe(true)
    expect(result.current.collisionRequest).toEqual([
      '/home/tester/.zshrc',
      '/home/tester/.bashrc',
    ])
    // The run is still in flight — resolving the collision is a separate call, not something
    // that itself ends the mutation.
    expect(result.current.status).toBe('running')
  })

  it('resolving yes/no calls resolve_collision, clears the pending request, and the run then completes', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() =>
      run.channel().onmessage({ type: 'collisionRequest', paths: ['/home/tester/.zshrc'] }),
    )
    expect(result.current.awaitingCollisionAnswer).toBe(true)

    act(() => result.current.resolveCollision(true))

    expect(mockedInvoke).toHaveBeenCalledWith('resolve_collision', { overwrite: true })
    // The confirmation UI hides immediately — it doesn't wait for the worker to actually
    // resume and the run to resolve.
    expect(result.current.awaitingCollisionAnswer).toBe(false)
    expect(result.current.status).toBe('running')

    // Only once the worker (unblocked by the reply above) actually finishes does the mutation
    // resolve — driven here by the test resolving `run_restore`'s own promise, exactly as the
    // real backend would once `core::restore` returns.
    act(() => run.resolve(SUCCESS_OUTCOME))
    await waitFor(() => expect(result.current.status).toBe('success'))
  })

  it('declining a collision still drives the run to completion', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() =>
      run.channel().onmessage({ type: 'collisionRequest', paths: ['/home/tester/.zshrc'] }),
    )
    act(() => result.current.resolveCollision(false))

    expect(mockedInvoke).toHaveBeenCalledWith('resolve_collision', { overwrite: false })

    act(() =>
      run.resolve({
        type: 'success',
        data: {
          restoredCount: 2,
          failures: [],
          skippedCollisions: [{ path: '/home/tester/.zshrc' }],
          ownership: 'skippedNonRoot',
          reinstallScriptPath: '/home/tester/reinstall.sh',
          packageCounts: {},
          cancelled: false,
        },
      }),
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(result.current.outcome?.type).toBe('success')
    if (result.current.outcome?.type === 'success') {
      expect(result.current.outcome.data.skippedCollisions).toEqual([
        { path: '/home/tester/.zshrc' },
      ])
    }
  })

  // --- the cancel interaction (returns the partial outcome) ---------------------------------

  it('firing cancel calls cancel_restore, and the run still resolves through its own outcome with the partial result', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'restoring' }))
    act(() => result.current.cancel())

    expect(mockedInvoke).toHaveBeenCalledWith('cancel_restore')
    // Cancelling alone must not fabricate a terminal state — the run only ends once
    // `run_restore` itself resolves, exactly as `useBackupRun` requires for `cancel_backup`.
    expect(result.current.status).toBe('running')

    act(() =>
      run.resolve({
        type: 'success',
        data: {
          restoredCount: 5,
          failures: [{ path: '/home/tester/big-file', reason: 'not yet restored' }],
          skippedCollisions: [],
          ownership: 'restored',
          reinstallScriptPath: '/home/tester/reinstall.sh',
          packageCounts: { pacman: 3 },
          cancelled: true,
        },
      }),
    )

    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(result.current.outcome?.type).toBe('success')
    if (result.current.outcome?.type === 'success') {
      // The partial outcome the user can inspect: what landed (`restoredCount`), what didn't
      // (`failures`), and the `cancelled` marker distinguishing it from a full run.
      expect(result.current.outcome.data.cancelled).toBe(true)
      expect(result.current.outcome.data.restoredCount).toBe(5)
      expect(result.current.outcome.data.failures).toHaveLength(1)
    }
  })

  it('surfaces a real CoreError as the failed status with its message', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x', 'wrong-passphrase'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() =>
      run.resolve({
        type: 'failed',
        data: { kind: 'decryption_failed', message: 'failed to decrypt: bad passphrase' },
      }),
    )

    await waitFor(() => expect(result.current.status).toBe('failed'))
    expect(result.current.outcome).toEqual({
      type: 'failed',
      data: { kind: 'decryption_failed', message: 'failed to decrypt: bad passphrase' },
    })
  })

  it('surfaces a rejected already-running start as a mutation error, not a fabricated status', async () => {
    // The backend guard (issue #103 review fix): a second run refused via
    // `SerializableCoreError::already_running()` rejects the `invoke()` promise outright —
    // distinct from every other outcome, which resolves. The frontend must surface this
    // through `error`, not silently pretend nothing happened.
    mockedInvoke.mockRejectedValueOnce({
      kind: 'already_running',
      message: 'another run is already in progress',
    })
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))

    await waitFor(() => expect(result.current.error).not.toBeNull())
    expect(result.current.error).toEqual({
      kind: 'already_running',
      message: 'another run is already in progress',
    })
  })

  it('surfaces a worker panic as the internal status', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.resolve({ type: 'internal', data: 'boom' }))

    await waitFor(() => expect(result.current.status).toBe('internal'))
  })

  it('reset returns the hook to idle with fresh progress and no pending collision', async () => {
    const run = mockRunRestore()
    const { result } = renderHook(() => useRestore(), { wrapper })

    act(() => result.current.start('/backups/x'))
    await waitFor(() => expect(result.current.status).toBe('running'))
    act(() => run.channel().onmessage({ type: 'phase', phase: 'restoring' }))
    act(() =>
      run.channel().onmessage({ type: 'collisionRequest', paths: ['/home/tester/.zshrc'] }),
    )
    act(() => run.resolve(SUCCESS_OUTCOME))
    await waitFor(() => expect(result.current.status).toBe('success'))

    act(() => result.current.reset())

    expect(result.current.status).toBe('idle')
    expect(result.current.outcome).toBeNull()
    expect(result.current.collisionRequest).toBeNull()
    expect(result.current.progress).toEqual({ phase: null, entryDone: 0, entryTotal: 0 })
  })
})
