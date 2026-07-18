# The ratatui TUI is a single `App` reducer in `tui`; core stays synchronous and owns all filesystem content I/O

The interactive surface migrates off `inquire` (linear, forward-only, print-then-prompt) to a
full-screen `ratatui` app. This ADR fixes that app's architecture and confirms where the
core/tui boundary lands. It resolves [#47](https://github.com/payouri/linux-backup-tool/issues/47)
and is the architectural spine the implementation spec ([#53](https://github.com/payouri/linux-backup-tool/issues/53)) rests on.

Crate/version choices and the headless-testing story are settled separately in
[`docs/research/2026-07-18-ratatui-foundation.md`](../research/2026-07-18-ratatui-foundation.md)
(pin `ratatui = "0.30"` + `crossterm = "0.29"`; pure `update`/render for `TestBackend`).

## Decisions

### 1. One `App` reducer, in `tui`

A single `App` struct — living entirely in `crates/tui` — holds all application state, with a
`Screen` enum as its active route (wizard step, restore, run, dev select/where). The app is a
pure `update(state, Event) -> state` reducer plus a pure render function, so event logic is
unit-testable with hand-built `KeyEvent`s and rendering is assertable via `TestBackend` (per the
foundation note). Core's `SelectionModel` is owned as a field of `App` and mutated **only**
through its existing public API (`select_category`, `deselect_root`, `add_custom_path`, …). View
state — active screen, cursor, scroll, input buffers, the passphrase reveal toggle — lives in
`App` *around* the core model, never inside it. Core does not know `App` exists.

### 2. One shared runtime, seeded by an initial `Screen`

Terminal lifecycle (raw mode + alternate screen, panic hook, the crossterm read loop, resize
handling, teardown) is written **once** in a single runtime function parameterized by an initial
`Screen`. Each command seeds a different starting state — `backup` → the wizard flow, `restore` →
the restore screen, dev `select`/`where` → their single screen — and hands it to the runtime. The
`main.rs` command dispatch shrinks to "build the initial `App`, run it." This avoids duplicating
the fragile teardown/panic-hook machinery per command.

### 3. Wizard state persists across steps

`App` holds a `WizardState` aggregate (the `SelectionModel`, the `Encryption` choice, and the
WHERE location choice) that persists for the whole wizard rather than being scoped to one screen.
Each `WizardStep` is a *view over* this shared state, so moving backward re-renders an earlier
step against already-populated choices — the structural prerequisite for the confirmed
back-navigable wizard anchor. The transition and cross-step invalidation rules (e.g. whether
changing WHAT invalidates a downstream encryption auto-insist) are **not** decided here; they are
owned by [#49](https://github.com/payouri/linux-backup-tool/issues/49).

### 4. The RUN phase runs core on a worker thread; progress bridges over a channel — no async, no tokio

`core::backup()` is synchronous and blocking and drives `&mut dyn Progress` inline on its calling
thread. A full-screen app cannot call it directly from the event loop without freezing the frame
(no repaint, spinner, resize, or cancel) for the entire run. Instead:

- `core::backup()` runs on a `std::thread` worker, unchanged.
- A `tui`-side `Progress` implementation (`ChannelProgress`) forwards `on_phase` / `on_entry`
  notifications as messages over a `std::sync::mpsc` channel — a plain synchronous, non-blocking
  `send`.
- The event loop, during `Screen::Run`, polls both crossterm events (`event::poll(timeout)`) and
  the progress channel (`try_recv`) each tick, folds progress into `App`'s run-phase widget state,
  and repaints. The frame stays live.
- A terminal `Done(BackupOutcome)` / `Err` message transitions the screen. A worker-thread panic
  disconnects the channel; the loop reads that as a failed run and tears down cleanly.

This is **not** async: both sides are fully synchronous, joined by one channel carrying a small
message enum. `tokio` (or crossterm's async `EventStream`) is deliberately **not** adopted — a
single background job does not warrant an async runtime, and it would fight core's sync,
`#![forbid(unsafe_code)]`, no-runtime shape. Core's `Progress` contract is untouched; the threading
and channel adapter live entirely in `tui`, exactly where `progress.rs` already says they belong.

*Downstream (implementation, tracked in the spec):* `restore()` gains a `Progress` parameter so
restore — which has no progress surface today — routes through the same channel model.

### 5. ADR-0001 is reaffirmed, not amended

The migration touches **only** `crates/tui`: `App`, the runtime, the worker thread, and the
channel adapter are all tui-internal, and every core entry point stays UI-agnostic and unchanged.
The core/tui split of [ADR-0001](0001-core-tui-split-no-dedup-trait.md) holds as-is, so this is a
*new* ADR rather than an amendment to it. (The recursive drill-down work in
[#48](https://github.com/payouri/linux-backup-tool/issues/48) may change core's `Selection`/
`Manifest` domain model, but that is a core-internal change, not a boundary change, and likewise
does not amend ADR-0001.)

### 6. Core is the bridge for all filesystem *content* I/O

All filesystem *content* interaction — directory listing for custom-path autocomplete, size/
metadata probes, existence checks, and privilege-aware handling of unreadable directories — lives
in **core**, exposed as plain functions taking an explicit path argument (no trait, no injection
into `RunContext`: there is one consumer, so introducing an abstraction now would be premature per
ADR-0001). `tui` performs no filesystem content I/O; it renders what core returns and surfaces
core's errors (e.g. "permission denied") as UI state.

This is consistent with [ADR-0004](0004-core-runcontext-injection.md): that ADR forbids core
reading ambient **run facts** (`$HOME`, hostname, uid, the clock), *not* core doing bounded FS work
whose path arrives as an argument — which core already does in `scan.rs`. The one carve-out is
environment-variable resolution inside a user-typed path (`$FOO/bar`): env vars *are* ambient run
facts, so they stay resolved at the tui boundary and passed into core (as the existing resolver-
closure pattern already does), never read by core itself.

Autocomplete UX detail (debounce, suggestion count, hidden-file and privilege handling) is owned
by [#52](https://github.com/payouri/linux-backup-tool/issues/52).

*Downstream (implementation, tracked in the spec):* the existing tui-side FS probes
(`select_cmd.rs` subtree-size `metadata`, `restore_cmd.rs`'s `is_dir()` / `is_encrypted_backup`)
migrate behind core functions to honor this boundary.

### 7. Error handling and panic-safe teardown

The error-type split is unchanged: `anyhow` at the tui boundary, `thiserror` `CoreError` in core.
A `CoreError` raised by the worker thread arrives over the channel and renders as an in-`App` error
state, so failures appear inside the TUI rather than as a stack dump over a wrecked terminal. A
panic hook that restores the terminal (leaves the alternate screen, disables raw mode) before the
default handler runs is installed **once** in the shared runtime; teardown is never left to the end
of `main` alone.

## Consequences

- The implementation spec (#53) is authored against this spine and carries two named implementation
  follow-ups: the `restore()` `Progress` parameter (Decision 4) and the tui→core FS-probe migration
  (Decision 6).
- Design tickets [#49](https://github.com/payouri/linux-backup-tool/issues/49) (navigation),
  [#50](https://github.com/payouri/linux-backup-tool/issues/50) (adaptive layout),
  [#51](https://github.com/payouri/linux-backup-tool/issues/51) (keymap + reveal), and
  [#52](https://github.com/payouri/linux-backup-tool/issues/52) (autocomplete) are unblocked and
  resolve against this structure.
- A contributor will be tempted to reach for `tokio` or crossterm's async `EventStream` "to do the
  backup properly." Don't — Decision 4 is a thread + channel on purpose; async buys nothing for one
  background job and violates the no-premature-dependency and sync-core constraints.
- Adopting `ratatui` 0.30 moves the workspace to edition 2024 (MSRV 1.88; the workspace is already
  on `rust-version = "1.95"`, so the toolchain clears it — the edition bump is the only change).
