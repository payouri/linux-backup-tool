//! Progress reporting: a callback trait `tui` implements to drive per-phase progress bars.
//!
//! `core` stays synchronous with no async runtime — any channel-based adapter (for a
//! future concurrent UI) lives in `crates/tui`, not here.
//!
//! This is intentionally minimal for #19 — real progress-bar wiring is TUI work (#33).
//! It exists now so `backup`/`restore` have a place to report phase changes without
//! needing a signature change later.

/// A coarse phase within a backup or restore run, reported through [`Progress`].
///
/// A backup run reports `Scanning` → `Compressing` → (`Encrypting`) → `Finalizing`, where
/// `Encrypting` fires only when the backup is actually encrypted (it is omitted entirely for
/// a plaintext backup, so a plaintext run's caller never sees it). The two on-disk write
/// steps (Archive then Manifest) surface as the single `Finalizing` phase rather than as
/// separate write phases — the coarse phase set tracks what the RUN screen shows, not each
/// internal write. `VerifyingArchive` and `Restoring` are the restore-side phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Compressing,
    Encrypting,
    Finalizing,
    VerifyingArchive,
    Restoring,
}

/// Callback trait a caller (typically `crates/tui`) implements to observe run progress.
///
/// `core` never blocks on this trait's calls and never inspects a return value from
/// them — it's a one-way notification, not a control hook.
pub trait Progress {
    /// Called when a run enters a new phase.
    fn on_phase(&mut self, _phase: Phase) {}

    /// Called after each Entry is processed within a phase, with a running count and the
    /// total known at the time (totals may be approximate during scanning).
    fn on_entry(&mut self, _done: usize, _total: usize) {}
}

/// A [`Progress`] implementation that does nothing — the default for callers (and tests)
/// that don't care about progress reporting.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopProgress;

impl Progress for NoopProgress {}
