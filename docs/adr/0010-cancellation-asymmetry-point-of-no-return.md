# Cancellation inherits ADR-0002's asymmetry: backup cancels only before its point of no return, restore cancels throughout and keeps what landed

**Status:** Accepted · **Date:** 2026-07-26

[ADR-0002](0002-backup-restore-failure-asymmetry.md) settled how backup and restore *fail*
differently. It says nothing about how they *cancel*, and cancellation turns out to follow the same
asymmetry for the same reason — but that reasoning has lived only in doc comments on
`core::backup`, `CoreError::Cancelled`, and a `CANCELLABLE_PHASES` constant in the frontend, with no
single record. This ADR states it once.

Recording it matters because the backup half is genuinely surprising: **cancellation is honored at
exactly one checkpoint, and past it a cancel request cannot stop the run.** A reader who assumes
"Cancel stops the thing" will either wire a cancel affordance that misleads the user, or "fix" the
checkpoint placement without realising what the current placement buys. The `PendingProbe` decision
in [ADR-0009](0009-what-step-worker-model-debounced-spinner.md) Decision 4 already leaned on this
distinction — WHAT's probes get no cancel affordance while "RUN's backup... legitimately warrants
one" — without ever writing down what RUN's cancellation actually promises.

The GUI ([ADR-0008](0008-gui-tauri-entry-point-core-boundary.md)) is what made this reachable. It is
the only entry point that flips the token today; `crates/tui` constructs a `CancelToken` and never
cancels, so the terminal app has no cancel affordance at all.

## Decisions

### 1. Cancellation is cooperative, and `core` only ever reads the flag

A `CancelToken` is passed to `backup`/`restore` as its own parameter — deliberately not a
`RunContext` field, because ADR-0004's `RunContext` carries run *facts* fixed for the whole run
while this is live control input another thread mutates mid-run. It stays separate from
`Progress` too, so observation and control never share a channel and `Progress` stays strictly
one-way.

`core` polls the flag at documented checkpoints and never interrupts work already in progress.
Nothing in `core` ever flips it; the caller owning the run is the only side that does. It is
`std` atomics only, so `core`'s `forbid(unsafe_code)` stays intact and no dependency is added.

### 2. Backup: one checkpoint, in the cancel-safe front half

`archive::build` polls the token once per Entry at the top of its read loop — between
`Phase::Scanning` and `Phase::Compressing`, before any Archive byte exists. A cancellation seen
there returns `CoreError::Cancelled`, which rides ADR-0002's fail-fast cleanup path exactly as a
real error does: the freshly-created Backup dir is removed and nothing is left behind. A cancelled
backup is therefore never a partial Backup — the same guarantee ADR-0002 gives a failed one.

**Past that loop, cancellation is not honored.** The zstd finish flush, the encryption pass, and
the Archive and Manifest writes all commit bytes and are deliberately not cancellable. A cancel
requested there lets the run finish and report Success.

That placement is a real trade-off, not an oversight. Two things argue for it:

- **The front half is where the time is, and where a checkpoint exists naturally.** Reading
  potentially thousands of Entries dominates a run's wall clock and already loops per Entry. The
  commit phase is a single streaming flush with no per-Entry boundary, so honoring cancellation
  there would mean inventing a byte-count checkpoint inside the encoder for a much shorter window.
- **It buys the user nothing they don't already have.** The remedy for an unwanted Backup that
  finished is deleting one self-contained Backup dir. Cancelling mid-commit would have to do the
  same deletion, having also risked leaving a half-written Archive on a path where cleanup
  didn't run — the precise danger ADR-0002 exists to prevent.

### 3. Restore: cancellable to the last Entry, and it keeps what landed

`restore` polls the token before unpacking each tar entry, for the whole run — there is no
front-half restriction, because there is nothing to protect. A cancellation stops the loop and
returns a **successful** `RestoreOutcome` with `cancelled: true` plus everything already restored.
Files that landed stay; there is no rollback.

So restore has **no point of no return**, and no `Cancelled` outcome of its own — cancellation is a
flag on a partial success. This is ADR-0002's principle applied directly: restore is best-effort
recovery, so stopping early is just a smaller best effort, not a failed run.

### 4. A cancel affordance is shown only while it can do something

Because a late cancel would flip the flag and change nothing, the desktop app treats `Scanning` as
the only cancellable backup phase and withdraws the affordance once `Compressing` arrives. The
alternative — leaving the control live — is worse than an inert button: it would report a
cancellation to the user and then land an on-disk Backup they believed they had stopped.

The affordance is withdrawn on phase, not on outcome, so this is a UI-side rule derived from where
`core` actually polls. Any interface adding cancellation must derive it the same way rather than
trusting prose.

## Consequences

- The vocabulary is in `CONTEXT.md`: **Cancellation**, **Point of no return**, **Run outcome**.
  A future change to any checkpoint above changes those entries too.
- **The frontend must handle two outcome shapes.** A backup can end `Cancelled`; a restore cannot,
  and reports its cancellation inside a `Success`. That is a deliberate consequence of Decision 3,
  not an inconsistency to normalise away.
- **The terminal app has no cancel affordance** and its `CancelToken` is inert. Adding one is a
  real feature, not a wiring detail: it needs Decision 4's phase-derived rule reimplemented in the
  ratatui reducer. Worth a ticket rather than an opportunistic patch.
- **Nothing polls the token during a package-list capture or a Plasma copy**, only during the
  per-Entry archive read. A future Category whose capture is slow but not per-Entry would sit
  outside every checkpoint above and would need its own.
- If cancelling the commit phase is ever genuinely wanted, Decision 2 names what it costs: a
  checkpoint inside the encoder, plus a guarantee that a partially-written Archive can never
  survive the cancel path.
