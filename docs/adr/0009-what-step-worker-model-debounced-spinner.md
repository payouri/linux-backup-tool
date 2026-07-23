# The WHAT step gets a generic worker primitive and a 375ms debounced spinner, amending ADR-0005

**Status:** Accepted · **Date:** 2026-07-23

[ADR-0005](0005-ratatui-app-architecture-core-tui-boundary.md) Decision 4 established a
worker-thread + `mpsc` channel + `Event::Tick` pattern so the RUN screens stay responsive during
`core::backup()`. That pattern is single-purpose (`BackupWorker`, `run_screen.rs`) and gated by
`Screen::Run`/`Screen::Restore` only — the WHAT step (`what_screen.rs`) stays a fully synchronous
`update`/`render` reducer that never sees a `Tick`. This ADR extends the same worker+channel+Tick
shape to WHAT's own slow, disk-touching operations (`fs_probe::subtree_size`, `total_size`, a
taxonomy scan), behind a debounced spinner so operations that resolve quickly never flicker
anything. It resolves [#93](https://github.com/payouri/linux-backup-tool/issues/93), a child
ticket of the [WHAT-screen responsiveness map](https://github.com/payouri/linux-backup-tool/issues/88).
It settles the *mechanism* only — which specific ops use it is
[#96](https://github.com/payouri/linux-backup-tool/issues/96); where the spinner sits and what it
says is [#94](https://github.com/payouri/linux-backup-tool/issues/94).

Two of the four reported UX issues this map covers (the 40k-category enable freeze, the
category-switch stale breadcrumb) turned out to be pure algorithmic/state bugs, fixed without
threading ([#89](https://github.com/payouri/linux-backup-tool/issues/89),
[#90](https://github.com/payouri/linux-backup-tool/issues/90)). What's left — `stat`-class calls
against the filesystem — can't be fixed away algorithmically; a large Selection or a
never-before-seen directory can genuinely take real wall-clock time no in-memory fix removes.

## Decisions

### 1. A generic, op-agnostic primitive — not a hardcoded per-op worker

Unlike `BackupWorker` (one job type, because RUN only ever has one job in flight), the WHAT step
gains a single generic primitive — `PendingProbe<T>` — that spawns any `FnOnce() -> T + Send` on a
`std::thread`, returns `T` over an `mpsc::channel`, and records its own spawn `Instant`. WHAT
already has at least two candidate probe kinds (`#96`'s size probes, a possible future taxonomy
rescan), so a reusable primitive costs little more than a single-purpose one and avoids inventing a
new worker type per op. The *specific ops* wired through it are #96's decision, not this ADR's; a
small result enum (e.g. `WhatProbeResult::Size(PathBuf, u64)`, `::TotalSize(u64)`) lets one channel
carry whichever op kind was in flight, mirroring `RunMessage`'s shape.

### 2. WHAT joins the poll loop only while a probe is pending; `Tick` routes into the wizard

`App::needs_poll()` (`app.rs:233`) gains a third disjunct alongside backup/restore:
`wizard.what().has_pending_probe()`. `App::update`'s `Event::Tick` arm (`app.rs:257-267`), which
today only matches `View::Run`/`View::Restore`, gains a `View::Wizard` case that forwards the tick
to the active WHAT step. That drain does two things per tick, mirroring
`drain_backup_worker`/`RunScreen::tick()`: a non-blocking `try_recv()` that, on success, writes the
result into the relevant cache and clears the pending slot; and a `spinner_tick` bump so the frame
visibly stays alive. Outside a pending probe, WHAT never polls — `run_loop` falls back to blocking
redraw-on-event exactly as today.

**In-flight state is a single `Option<PendingProbe<WhatProbeResult>>`**, not a collection or
request-tracking map — one keystroke drives at most one op from the user's perspective. If a new
probe is requested while one is already pending (cursor lands on a different never-seen file before
the first resolves), the new request **replaces** the old one: the stale `PendingProbe` and its
receiver are dropped, no queueing.

### 3. The 375ms debounce is a plain elapsed-time check; no new timer

`PendingProbe::started: Instant` is captured at spawn (tui-side `Instant::now()` — this never
touches `core`, so it doesn't violate ADR-0004's ambient-clock rule). The spinner becomes visible
exactly when `started.elapsed() >= Duration::from_millis(375)`, re-evaluated every `render()` call
while the probe is pending; below that threshold WHAT renders identically to today, so a probe that
resolves in 50ms never flashes anything. The frame is kept live by reusing the existing
`POLL_INTERVAL` (`runtime.rs:36`, 80ms) — no dedicated debounce timer or new interval constant.
80ms is frequent enough to notice the 375ms threshold within a handful of ticks and to drive a
smooth spinner once revealed.

### 4. Input is never blocked; no explicit cancel

A pending probe only affects the rendering of the specific value it's computing — the rest of WHAT
(cursor movement, other panes, other selections) stays fully interactive the entire time, and the
reducer keeps handling every key event immediately regardless of pending state. This is the point
of the mechanism: it must not reproduce the class of freeze #89 fixed. There is no explicit
cancel affordance. WHAT probes are read-only, `stat`-class calls — cheap, not meaningfully
interruptible mid-flight, and self-terminating: an abandoned worker finishes its blocking call,
attempts a `send` nobody receives, and exits. This differs deliberately from RUN's backup, a long,
user-visible, resource-mutating operation that legitimately warrants a "Cancel" affordance — nothing
in WHAT's probe set rises to that stakes level.

## Consequences

- [Size-probe cost decision (#96)](https://github.com/payouri/linux-backup-tool/issues/96) consumes
  `PendingProbe<T>` directly to decide which of `total_size`/`subtree_size` are cached, threaded, or
  both.
- [Loader UX prototype (#94)](https://github.com/payouri/linux-backup-tool/issues/94) designs the
  spinner's placement/copy/dismissal against the input-never-blocked, no-cancel mechanism fixed
  here.
- A contributor extending WHAT with a new slow op should reach for `PendingProbe<T>` rather than a
  bespoke worker/channel pair or, worse, a synchronous call on the render path.
- `Event::Tick`'s doc comment (`app.rs:81-84`, "every non-polling screen never sees one") needs
  updating: WHAT is now a **conditionally**-polling screen, polling only while a probe is in flight.
- Implementation (wiring `PendingProbe<T>`, the `needs_poll()`/`Tick` routing, and `size_cache`
  writes on drain) is a downstream effort — this map produces decisions, not code.
