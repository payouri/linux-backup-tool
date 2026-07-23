import type { SerializableCoreError } from '@/types/errors'

/**
 * Mirrors `crates/gui/src/run.rs`'s `PhaseDto` — the wire form of `core::progress::Phase`
 * (issue #102). `VerifyingArchive`/`Restoring` are restore-only phases a backup run never
 * emits; they're still part of the union so this type matches the Rust enum exhaustively
 * rather than silently narrowing it.
 */
export type BackupPhase =
  | 'scanning'
  | 'compressing'
  | 'encrypting'
  | 'finalizing'
  | 'verifyingArchive'
  | 'restoring'

/**
 * Mirrors `crates/gui/src/run.rs`'s `ProgressEventDto` — one message sent over the RUN
 * command's progress `Channel` (issue #102, ADR-0008 Decision 3). Notifications only, never a
 * terminal result — that arrives as the `run_backup` command's own resolved value
 * ({@link RunOutcome}), not over this channel.
 */
export type ProgressEvent =
  | { type: 'phase'; phase: BackupPhase }
  | { type: 'entry'; done: number; total: number }

/**
 * Mirrors `crates/gui/src/run.rs`'s `ManifestSummaryDto`: counts, not the whole Manifest.
 */
export interface ManifestSummaryDto {
  entryCount: number
  encrypted: boolean
  /** Per-manager Package counts (manager name -> count). */
  packages: Record<string, number>
  vanishedCount: number
}

/** Mirrors `crates/gui/src/run.rs`'s `BackupOutcomeDto`: where the Backup dir landed plus the
 * Manifest summary. */
export interface BackupOutcomeDto {
  backupDir: string
  manifest: ManifestSummaryDto
}

/**
 * Mirrors `crates/gui/src/run.rs`'s `RunOutcomeDto` — the one boundary enum the frontend
 * matches exhaustively (ADR-0008 Decision 3). Adjacently tagged (`type`/`data`) to match the
 * Rust side's `#[serde(tag = "type", content = "data")]`: `success`/`failed`/`internal` carry
 * a `data` payload, `cancelled` carries none.
 */
export type RunOutcome =
  | { type: 'success'; data: BackupOutcomeDto }
  | { type: 'cancelled' }
  | { type: 'failed'; data: SerializableCoreError }
  | { type: 'internal'; data: string }
