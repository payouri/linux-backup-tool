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

## Behavior — backup/restore policy and feature capture

- [`0002`](0002-backup-restore-failure-asymmetry.md) — backup fails fast and cleans up; restore is
  best-effort and reports per-file failures.
- [`0003`](0003-plasma-raw-config-copy-not-konsave.md) — Plasma capture copies raw config via a
  curated allowlist; no `konsave` dependency.
