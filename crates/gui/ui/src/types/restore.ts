import type { SerializableCoreError } from '@/types/errors'
import type { BackupPhase } from '@/types/run'

/**
 * Mirrors `crates/gui/src/restore.rs`'s `RestoreProbeDto` (issue #103, stories 39/40): whether
 * a picked Backup dir is a real, reachable directory and — only if it is — whether it holds an
 * encrypted Backup. Drives whether the passphrase field is shown at all.
 */
export interface RestoreProbeDto {
  exists: boolean
  encrypted: boolean
}

/**
 * Mirrors `crates/gui/src/restore.rs`'s `RestoreEventDto` — one message sent over
 * `run_restore`'s per-invocation `Channel` (issue #103): the same Phase/Entry progress
 * notifications backup's `ProgressEvent` carries, plus `collisionRequest` — the one-shot "here
 * is the full colliding-path list" message the blocked worker sends before it blocks on
 * `resolve_collision`'s reply.
 */
export type RestoreEvent =
  | { type: 'phase'; phase: BackupPhase }
  | { type: 'entry'; done: number; total: number }
  | { type: 'collisionRequest'; paths: string[] }

/** Mirrors `crates/gui/src/restore.rs`'s `OwnershipOutcomeDto` (story 46). */
export type OwnershipOutcome = 'restored' | 'skippedNonRoot'

/** Mirrors `crates/gui/src/restore.rs`'s `RestoreFailureDto` (story 48). */
export interface RestoreFailureDto {
  path: string
  reason: string
}

/** Mirrors `crates/gui/src/restore.rs`'s `SkippedCollisionDto` (story 43). */
export interface SkippedCollisionDto {
  path: string
}

/**
 * Mirrors `crates/gui/src/restore.rs`'s `RestoreOutcomeDto`: every fact the report screen
 * needs, `restoredCount` a count rather than the full path list (mirroring backup's
 * `ManifestSummaryDto`).
 */
export interface RestoreOutcomeDto {
  restoredCount: number
  failures: RestoreFailureDto[]
  skippedCollisions: SkippedCollisionDto[]
  ownership: OwnershipOutcome
  reinstallScriptPath: string
  /** Per-manager Package counts (manager name -> count). */
  packageCounts: Record<string, number>
  /** Set when the run stopped early via cancellation — the `Ok`-side partial outcome (ADR-0008
   * Decision 3's restore asymmetry: no separate `Cancelled` terminal variant). */
  cancelled: boolean
}

/**
 * Mirrors `crates/gui/src/restore.rs`'s `RestoreRunOutcomeDto` — the one boundary enum the
 * frontend matches exhaustively for a restore run. No `cancelled` variant of its own: a
 * cancelled run is still `success`, with `data.cancelled` set (unlike backup's `RunOutcome`).
 */
export type RestoreRunOutcome =
  | { type: 'success'; data: RestoreOutcomeDto }
  | { type: 'failed'; data: SerializableCoreError }
  | { type: 'internal'; data: string }
