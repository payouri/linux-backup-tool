import { useCallback, useState } from 'react'
import { useMutation } from '@tanstack/react-query'

import { Channel, invoke } from '@/lib/ipc'
import type { BackupPhase } from '@/types/run'
import type { RestoreEvent, RestoreRunOutcome } from '@/types/restore'

/** The Restore screen's live progress state (issue #103): the current phase (`null` until the
 * worker's first message) and the per-Entry counts within it — the restore analogue of
 * `useRun.ts`'s `RunProgress`. */
export interface RestoreProgress {
  phase: BackupPhase | null
  entryDone: number
  entryTotal: number
}

const INITIAL_PROGRESS: RestoreProgress = { phase: null, entryDone: 0, entryTotal: 0 }

/** A single, terminal-outcome-agnostic status label the Restore screen renders from. Unlike
 * backup's `RunStatus`, there is no `cancelled` label of its own: a cancelled restore is still
 * `success`, with `outcome.data.cancelled` set (ADR-0008 Decision 3's restore asymmetry — no
 * cancel-safe front half, so cancellation is `Ok`-side data, never a distinct terminal shape). */
export type RestoreStatus = 'idle' | 'running' | 'success' | 'failed' | 'internal'

/**
 * The Restore flow's frontend half (issue #103): one `useMutation` per run, mirroring
 * `useRun.ts`'s `useBackupRun` shape exactly, plus the collision request→response interaction
 * `CollisionPolicy::Confirm`'s IPC bridge introduces. A `collisionRequest` arriving over the
 * mocked/real channel surfaces the confirmation (the colliding-path list); `resolveCollision`
 * delivers the user's single yes/no via the separate `resolve_collision` command, unblocking
 * the worker thread `run_restore` has parked on its reply channel.
 */
export function useRestore() {
  const [progress, setProgress] = useState<RestoreProgress>(INITIAL_PROGRESS)
  const [collisionRequest, setCollisionRequest] = useState<string[] | null>(null)

  const mutation = useMutation<
    RestoreRunOutcome,
    unknown,
    { backupDir: string; passphrase: string | null }
  >({
    // A stable key (issue #103 review fix), the restore counterpart of `useRun.ts`'s
    // `['run-backup']` — lets `App.tsx` observe "a restore is running" via `useIsMutating`
    // without lifting this hook's own instance up to that level.
    mutationKey: ['run-restore'],
    mutationFn: ({ backupDir, passphrase }) => {
      setProgress(INITIAL_PROGRESS)
      setCollisionRequest(null)
      const channel = new Channel<RestoreEvent>()
      channel.onmessage = (event) => {
        if (event.type === 'phase') {
          setProgress((previous) => ({ ...previous, phase: event.phase }))
        } else if (event.type === 'entry') {
          setProgress((previous) => ({
            ...previous,
            entryDone: event.done,
            entryTotal: event.total,
          }))
        } else {
          setCollisionRequest(event.paths)
        }
      }
      return invoke<RestoreRunOutcome>('run_restore', { backupDir, passphrase, channel })
    },
  })

  const start = useCallback(
    (backupDir: string, passphrase?: string) => {
      mutation.mutate({ backupDir, passphrase: passphrase ?? null })
    },
    [mutation],
  )

  /** Delivers the user's single yes/no into the blocked worker's reply channel (issue #103):
   * the *other* half of the request→response bridge, a separate command from `run_restore` so
   * the reply never shares a channel with the one-way progress stream. Clears the pending
   * request immediately — the confirmation UI hides the moment an answer is given, regardless
   * of how long the worker takes to actually resume. */
  const resolveCollision = useCallback((overwrite: boolean) => {
    setCollisionRequest(null)
    void invoke('resolve_collision', { overwrite })
  }, [])

  /** Per-Entry cancellation for restore (issue #103): flips the backend's `CancelToken` via
   * its own command, fire-and-forget — the run itself only ends through `run_restore`'s own
   * resolution (a `success` outcome with `data.cancelled` set). */
  const cancel = useCallback(() => {
    void invoke('cancel_restore')
  }, [])

  const reset = useCallback(() => {
    mutation.reset()
    setProgress(INITIAL_PROGRESS)
    setCollisionRequest(null)
  }, [mutation])

  const status: RestoreStatus = mutation.isPending
    ? 'running'
    : (mutation.data?.type ?? 'idle')

  return {
    status,
    progress,
    collisionRequest,
    outcome: mutation.data ?? null,
    error: mutation.error,
    start,
    resolveCollision,
    cancel,
    reset,
    /** Whether the run is currently blocked waiting on a collision answer — the confirmation
     * modal's own visibility rule. */
    awaitingCollisionAnswer: collisionRequest !== null,
  }
}
