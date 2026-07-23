import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { ProgressEvent, RunOutcome } from '@/types/run'

/**
 * A minimal stand-in for `@tauri-apps/api/core`'s real `Channel` (mirrors the spec #97 Testing
 * Decisions' "a fake invoke/Channel"): a settable `onmessage` the test drives directly, exactly
 * as Tauri's real bridge would deliver each `ProgressEventDto` send. Declared via `vi.hoisted`
 * because `vi.mock`'s factory below is itself hoisted above any plain top-level declaration.
 */
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

import { useBackupRun } from './useRun'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

/** Wires `mockedInvoke` so `run_backup` returns a promise this test controls the resolution
 * of, capturing the `Channel` instance the hook constructed so the test can drive its
 * `onmessage` the way the real Tauri bridge would. `cancel_backup` always resolves
 * immediately — its own promise carries no information the hook reads. */
interface CapturedChannel {
  onmessage: (event: ProgressEvent) => void
}

function mockRunBackup() {
  let resolveRun!: (outcome: RunOutcome) => void
  let capturedChannel: CapturedChannel | undefined

  mockedInvoke.mockImplementation((command: string, args?: unknown) => {
    if (command === 'run_backup') {
      capturedChannel = (args as { channel: CapturedChannel } | undefined)?.channel
      return new Promise<RunOutcome>((resolve) => {
        resolveRun = resolve
      })
    }
    if (command === 'cancel_backup') {
      return Promise.resolve(undefined)
    }
    throw new Error(`unexpected invoke: ${command}`)
  })

  return {
    resolve: (outcome: RunOutcome) => resolveRun(outcome),
    channel: () => {
      if (!capturedChannel) {
        throw new Error('run_backup was not invoked yet — no Channel captured')
      }
      return capturedChannel
    },
  }
}

describe('useBackupRun', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('starts idle, with no outcome and nothing to cancel', () => {
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    expect(result.current.status).toBe('idle')
    expect(result.current.outcome).toBeNull()
    expect(result.current.canCancel).toBe(false)
  })

  it('folds phase and entry progress events from the mocked channel', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'scanning' }))
    expect(result.current.progress.phase).toBe('scanning')

    act(() => run.channel().onmessage({ type: 'entry', done: 6, total: 12 }))
    expect(result.current.progress.entryDone).toBe(6)
    expect(result.current.progress.entryTotal).toBe(12)

    act(() => run.resolve({ type: 'cancelled' }))
    await waitFor(() => expect(result.current.status).toBe('cancelled'))
  })

  it('firing cancel drives the hook to the neutral Cancelled state, not a failure', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'scanning' }))
    result.current.cancel()
    expect(mockedInvoke).toHaveBeenCalledWith('cancel_backup')

    // The run itself only ends once `run_backup`'s own promise resolves (core's fail-fast
    // cleanup runs before that happens) — cancel() alone must not fabricate a terminal state.
    expect(result.current.status).toBe('running')

    act(() => run.resolve({ type: 'cancelled' }))

    await waitFor(() => expect(result.current.status).toBe('cancelled'))
    expect(result.current.outcome).toEqual({ type: 'cancelled' })
  })

  it('hides the cancel affordance the moment compressing arrives — core stops polling the token at the end of Scanning', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'scanning' }))
    expect(result.current.canCancel).toBe(true)

    // `crates/core/src/archive.rs` polls `cancel.is_cancelled()` only inside the Scanning
    // read loop, between `on_phase(Scanning)` and `on_phase(Compressing)` — once `compressing`
    // arrives, cancelling would be a no-op that still lets the run finish and land an on-disk
    // backup, so the affordance must already be gone here, not only at `finalizing`.
    act(() => run.channel().onmessage({ type: 'phase', phase: 'compressing' }))
    expect(result.current.canCancel).toBe(false)

    act(() => run.resolve({ type: 'success', data: { backupDir: '/x', manifest: {
      entryCount: 1, encrypted: false, packages: {}, vanishedCount: 0,
    } } }))
    await waitFor(() => expect(result.current.status).toBe('success'))
    // Terminal either way: no cancel affordance once the run has ended.
    expect(result.current.canCancel).toBe(false)
  })

  it('never shows the cancel affordance during encrypting either', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() => run.channel().onmessage({ type: 'phase', phase: 'scanning' }))
    expect(result.current.canCancel).toBe(true)

    act(() => run.channel().onmessage({ type: 'phase', phase: 'encrypting' }))
    expect(result.current.canCancel).toBe(false)

    act(() => run.resolve({ type: 'success', data: { backupDir: '/x', manifest: {
      entryCount: 1, encrypted: true, packages: {}, vanishedCount: 0,
    } } }))
    await waitFor(() => expect(result.current.status).toBe('success'))
  })

  it('surfaces a real CoreError as the failed status with its message', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))

    act(() =>
      run.resolve({
        type: 'failed',
        data: { kind: 'empty_selection', message: 'selection is empty: nothing to back up' },
      }),
    )

    await waitFor(() => expect(result.current.status).toBe('failed'))
    expect(result.current.outcome).toEqual({
      type: 'failed',
      data: { kind: 'empty_selection', message: 'selection is empty: nothing to back up' },
    })
  })

  it('reset returns the hook to idle with fresh progress', async () => {
    const run = mockRunBackup()
    const { result } = renderHook(() => useBackupRun(), { wrapper })

    act(() => result.current.start('/home/tester'))
    await waitFor(() => expect(result.current.status).toBe('running'))
    act(() => run.channel().onmessage({ type: 'phase', phase: 'scanning' }))
    act(() => run.resolve({ type: 'internal', data: 'boom' }))
    await waitFor(() => expect(result.current.status).toBe('internal'))

    act(() => result.current.reset())

    expect(result.current.status).toBe('idle')
    expect(result.current.outcome).toBeNull()
    expect(result.current.progress).toEqual({ phase: null, entryDone: 0, entryTotal: 0 })
  })
})
