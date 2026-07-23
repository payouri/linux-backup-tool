import { useCallback, useState } from 'react'
import { useMutation } from '@tanstack/react-query'

import { Channel, invoke } from '@/lib/ipc'
import type { BackupPhase, ProgressEvent, RunOutcome } from '@/types/run'

/** The RUN screen's live progress state (issue #102): the current phase (`null` until the
 * worker's first message) and the per-Entry counts within it. */
export interface RunProgress {
  phase: BackupPhase | null
  entryDone: number
  entryTotal: number
}

const INITIAL_PROGRESS: RunProgress = { phase: null, entryDone: 0, entryTotal: 0 }

/**
 * Backup phases the cancel affordance stays live through — verified directly against where
 * `core` actually polls the `CancelToken`, not against ADR-0008's looser "WritingArchive"
 * prose. `crates/core/src/archive.rs`'s `build` polls `cancel.is_cancelled()` at exactly one
 * site: inside the per-Entry read loop, between `on_phase(Scanning)` and `on_phase(Compressing)`.
 * Nothing polls it again afterward — not the zstd finish flush, not `Encrypting`
 * (`crates/gui/src/run.rs`'s `PhaseDto`), not the `Finalizing` disk writes. So `Scanning` is the
 * *only* genuinely cancellable phase; once a `compressing` (or later) phase message arrives, a
 * click on Cancel would flip the token and accomplish nothing but let the run finish and land
 * an on-disk backup the user believed they'd cancelled — worse than a merely-inert button, an
 * actively misleading one. Hence a strict single-phase set here rather than the wider
 * scanning/compressing/encrypting set an earlier draft used.
 */
const CANCELLABLE_PHASES: ReadonlySet<BackupPhase> = new Set(['scanning'])

/** Whether the cancel affordance should still be shown for the given live phase (`null` — no
 * phase message has arrived yet — is cancellable, matching a run's opening Scanning window). */
function isCancellablePhase(phase: BackupPhase | null): boolean {
  return phase === null || CANCELLABLE_PHASES.has(phase)
}

/** A single, terminal-outcome-agnostic status label the RUN screen renders from — derived from
 * the mutation's own lifecycle plus the resolved {@link RunOutcome}'s tag, never duplicated as
 * separate state of its own. */
export type RunStatus = 'idle' | 'running' | 'success' | 'cancelled' | 'failed' | 'internal'

/**
 * The RUN model's frontend half (issue #102, ADR-0008 Decision 3): one `useMutation` per run —
 * the `run_backup` command's resolved value *is* the terminal `RunOutcome`, matched
 * exhaustively by {@link RunStatus}/`outcome`. A per-call `Channel` carries progress
 * notifications only (`onmessage` folds into `progress`, mirrored nowhere else); a separate
 * `cancel_backup` invoke flips the backend's `CancelToken` — cancellation is a distinct call,
 * never a message sent back over the same channel, keeping the transport as one-way as
 * `core::Progress` itself.
 */
export function useBackupRun() {
  const [progress, setProgress] = useState<RunProgress>(INITIAL_PROGRESS)

  const mutation = useMutation<RunOutcome, unknown, { location: string }>({
    // A stable key (issue #103 review fix) so `App.tsx` can observe "a backup is running"
    // via `useIsMutating` without lifting this hook's own instance up to that level — the
    // toggle-disabling guard against switching away mid-run.
    mutationKey: ['run-backup'],
    mutationFn: ({ location }) => {
      setProgress(INITIAL_PROGRESS)
      const channel = new Channel<ProgressEvent>()
      channel.onmessage = (event) => {
        setProgress((previous) =>
          event.type === 'phase'
            ? { ...previous, phase: event.phase }
            : { ...previous, entryDone: event.done, entryTotal: event.total },
        )
      }
      return invoke<RunOutcome>('run_backup', { location, channel })
    },
  })

  const start = useCallback(
    (location: string) => {
      mutation.mutate({ location })
    },
    [mutation],
  )

  /** Front-half-only cancellation (issue #102): flips the backend's `CancelToken` via its own
   * command. Fire-and-forget — the run itself still ends through the normal `run_backup`
   * resolution (`RunOutcome::Cancelled`), not through this call's own promise. */
  const cancel = useCallback(() => {
    void invoke('cancel_backup')
  }, [])

  const reset = useCallback(() => {
    mutation.reset()
    setProgress(INITIAL_PROGRESS)
  }, [mutation])

  const status: RunStatus = mutation.isPending
    ? 'running'
    : (mutation.data?.type ?? 'idle')

  return {
    status,
    progress,
    outcome: mutation.data ?? null,
    error: mutation.error,
    start,
    cancel,
    reset,
    /** The cancel affordance's own visibility rule (issue #102's story 33): only while
     * running, and only up through the front-half phases. */
    canCancel: mutation.isPending && isCancellablePhase(progress.phase),
  }
}
