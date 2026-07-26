# Architectural Decision Records

Each ADR is an append-only record of one decision at a point in time. Read the ones touching your
area before changing it, and flag contradictions rather than silently overriding. When a decision is
reversed, add a new ADR and mark the old one `Superseded by 000N` — don't edit or delete it.

## Boundary — what lives in `core` vs `tui`, and no premature abstraction

- [`0001`](0001-core-tui-split-no-dedup-trait.md) — `core`/`tui` Cargo split; no `ArchiveBackend`
  trait until a second backend exists.
- [`0004`](0004-core-runcontext-injection.md) — `core` takes a full injected `RunContext` and reads
  nothing ambient. Extends 0001.
- [`0005`](0005-ratatui-app-architecture-core-tui-boundary.md) — the ratatui TUI is one `App`
  reducer in `tui`; core stays synchronous and owns all filesystem content I/O. Reaffirms 0001.
- [`0006`](0006-recursive-drill-down-exclusion-model.md) — recursive drill-down reuses the flat
  two-set exclusion model; no Manifest change, no Profile migration. Builds on 0001 and 0005.
- [`0007`](0007-wizard-navigation-cross-step-invalidation.md) — the wizard is a completion-frontier
  stepper with targeted WHAT→encryption invalidation; a set passphrase is a safe harbor. Fills the
  transition/invalidation hole 0005 left open.
- [`0008`](0008-gui-tauri-entry-point-core-boundary.md) — the second entry point is a Tauri GUI in
  `crates/gui` consuming a synchronous `core` over a JSON IPC boundary. Reaffirms 0001; the GUI
  analogue of 0005.
- [`0009`](0009-what-step-worker-model-debounced-spinner.md) — the WHAT step gets a generic
  `PendingProbe<T>` worker primitive and a 375ms debounced spinner for slow disk probes. Amends
  0005's worker+channel+Tick pattern.

## Behavior — backup/restore policy and feature capture

- [`0002`](0002-backup-restore-failure-asymmetry.md) — backup fails fast and cleans up; restore is
  best-effort and reports per-file failures.
- [`0003`](0003-plasma-raw-config-copy-not-konsave.md) — Plasma capture copies raw config via a
  curated allowlist; no `konsave` dependency.
- [`0010`](0010-cancellation-asymmetry-point-of-no-return.md) — cancellation inherits 0002's
  asymmetry: backup cancels only before its point of no return and leaves nothing behind; restore
  cancels throughout and keeps what landed. Extends 0002.
