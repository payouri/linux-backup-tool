# Crossterm event coalescing / render throttling for long-press latency

Research resolving ticket [#91](https://github.com/payouri/linux-backup-tool/issues/91)
(child of map [#88](https://github.com/payouri/linux-backup-tool/issues/88)).
Scope: surface the facts the #3-fix decision waits on — whether the crossterm event queue
can be drained non-blockingly to collapse a burst of repeated motion keys into one render,
whether Linux terminals emit `KeyEventKind::Repeat`, and the idiomatic ratatui pattern for
input coalescing / render throttling. **This note gathers facts only; it does not pick the fix.**

Versions in use (confirmed via `crates/tui/Cargo.toml` + `Cargo.lock`, 2026-07-22):
**`crossterm` 0.29.0**, **`ratatui` 0.30** (`ratatui-crossterm` bundles the same crossterm 0.29).
Docs/API facts confirmed on docs.rs and the crossterm/ratatui GitHub repos on **2026-07-22**.

## Current behavior (baseline)

`crates/tui/src/runtime.rs` runs a redraw-on-event loop: each interactive frame does
`terminal.draw(...)` then blocks on `read_event()` → `event::read()`, folds **one** event into
state, and repaints. `read_event` forwards only `KeyEventKind::Press` keys and `Resize`; all
other events (non-`Press` keys, mouse) map to `None`:

```rust
CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => Some(Event::Key(key)),
```

The app **does not** push any keyboard enhancement flags — `init_terminal` only enables raw
mode + enters the alternate screen (grep for `PushKeyboardEnhancementFlags` /
`KeyboardEnhancementFlags` / `REPORT_` across `crates/` returns nothing). The `use-dev-tty`
crossterm feature is **not** enabled either.

Consequence: holding ↑/↓ produces a *stream of `Press` events* (see Q2), one drained per loop
turn, each driving a full `handle_key` + repaint — so renders queue behind the input burst.

## Q1 — Can the queue be drained non-blockingly to coalesce a burst into one render?

**Yes.** crossterm exposes exactly the primitives needed:

- `event::poll(timeout)` "checks if there is an `Event` available" without necessarily reading
  it; returns `Ok(true)` if one is ready, `Ok(false)` if none arrives within `timeout`. A
  **zero** duration "says that the `poll` function must return immediately with an `Event`
  availability information" — i.e. a true non-blocking check.
  ([docs.rs, crossterm 0.29 `event::poll`](https://docs.rs/crossterm/latest/crossterm/event/fn.poll.html))
- The docs guarantee that after `poll` returns `Ok(true)`, "a subsequent call to the `read`
  function won't block."
- `event::read()` "blocks until an `Event` is available"; the docs explicitly say "Combine it
  with the `poll` function to get non-blocking reads."
  ([docs.rs, crossterm 0.29 `event::read`](https://docs.rs/crossterm/latest/crossterm/event/fn.read.html))

So a drain loop such as `while event::poll(Duration::ZERO)? { let e = event::read()?; /* fold */ }`
is a supported way to consume every currently-buffered event and repaint **once** afterward.
This is the synchronous analogue of the redraw-on-event loop already in `runtime.rs`
(`poll_event`/`read_event` already wrap this poll+read pair, just without the drain-to-empty).

**Caveat (does not apply here, but flag it):** crossterm
[#839](https://github.com/crossterm-rs/crossterm/issues/839) reports that
`event::poll(Duration::ZERO)` can incorrectly return `false` while an event is buffered —
**but only when the `use-dev-tty` feature is enabled** (it reads fd 0 directly). This repo does
not enable `use-dev-tty`, so the default poll path — which the issue confirms "works as
expected" — is what's in play.

**Idiomatic ratatui pattern:** ratatui itself owns the render, not the event loop — "the user
handles the event loop … and re-draws the entire UI on each iteration" and must bring their own
input source (crossterm) ([Ratatui — Event Handling](https://ratatui.rs/concepts/event-handling/),
[Ratatui — FAQ](https://ratatui.rs/faq/)). The ratatui **Event Handling** concepts page covers
*architecture* (centralized vs. distributed handlers) and does **not** prescribe coalescing or
throttling. Where ratatui docs *do* address input floods is the async recipe (see Q3):
decouple a capped **render/frame rate** from the input tick rather than rendering per event.
There is no single canonical "drain-and-coalesce" recipe in the synchronous ratatui docs; the
`poll(ZERO)` drain loop above is the sync equivalent of the async frame-rate cap.

## Q2 — Does crossterm emit `KeyEventKind::Repeat` for held keys on Linux?

**Not on a plain terminal.** `KeyEventKind` has three variants (`Press`, `Repeat`, `Release`),
but `Repeat`/`Release` are a **kitty keyboard protocol** feature that must be explicitly
requested and is only honored by supporting terminals. From crossterm's
`KeyboardEnhancementFlags`
([docs.rs, crossterm 0.29](https://docs.rs/crossterm/latest/crossterm/event/struct.KeyboardEnhancementFlags.html)):

- `REPORT_EVENT_TYPES` — "Add extra events with `KeyEvent.kind` set to
  `KeyEventKind::Repeat` or `KeyEventKind::Release` when keys are autorepeated or released."
- `REPORT_ALL_KEYS_AS_ESCAPE_CODES` — "Represent all keyboard events as CSI-u sequences. **This
  is required to get repeat/release events for plain-text keys.**"

These flags are opt-in via `PushKeyboardEnhancementFlags` and only take effect on kitty-protocol
terminals (kitty, foot, recent WezTerm/Ghostty); on ordinary terminals input reports **key
press only**. This app pushes **no** enhancement flags, so on Linux today it will **never** see
`KeyEventKind::Repeat`.

**Where the burst actually comes from:** with no enhancement flags, a held key is repeated by
the OS/terminal *autorepeat*, which re-sends the key's byte sequence. crossterm parses each
occurrence as a fresh `KeyEventKind::Press`. So the flood of events during a long-press is a
flood of **`Press`** events — the current `kind == Press` filter is **not** dropping them, and
is **not** the cause of (nor a lever on) the latency. (For contrast: on Windows the same key
reports both a `Press` and a `Release`, which is why the Press-only filter exists — see
[ratatui #347](https://github.com/ratatui/ratatui/issues/347),
[crossterm #797](https://github.com/crossterm-rs/crossterm/issues/797).)

**Would consuming `Repeat` help or hurt latency?**
- On the terminals this app targets today (no flags pushed): **moot** — no `Repeat` is emitted,
  so forwarding it would change nothing.
- If enhancement flags *were* pushed: autorepeat would arrive tagged `Repeat` instead of
  `Press`, at the **same rate** — consuming `Repeat` as if it were `Press` neither reduces the
  event count nor the latency. The only latency lever is coalescing/throttling the burst
  (Q1/Q3), which is **orthogonal** to Press-vs-Repeat. Distinguishing `Repeat` *could* let a
  fix collapse held-key motion specifically, but it costs a terminal-capability-dependent code
  path (works only on kitty-protocol terminals) for no gain the queue-drain doesn't already
  give portably.

## Q3 — Prior art in ratatui apps for coalescing / render-throttling

The blessed ratatui pattern for input floods is **decoupling a capped render rate from the
event/tick rate**, not rendering once per input:

- **Ratatui async recipe** ([Tui with Terminal and EventHandler](https://ratatui.rs/recipes/apps/terminal-and-event-handler/),
  [Async Event Stream](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/)):
  a `tokio::select!` loop drives independent `tick_rate` (~4/s) and `frame_rate` (~30/s)
  intervals plus a crossterm `EventStream`, emitting separate `Event::Tick` and `Event::Render`
  messages. Rendering is gated on the `Render` tick, so a burst of input cannot force more than
  ~30 repaints/second regardless of how fast events arrive.
- **`d-holguin/async-ratatui`** ([repo](https://github.com/d-holguin/async-ratatui)): calls out
  the exact failure mode — "If frequent updates occur (like a constant stream of inputs), you
  might need to limit the number of renders in a given time frame to prevent starvation of
  other tasks" — and solves it with separate `tick_rate` (~10/s) and `frame_rate` (~30/s)
  interval timers so the frame interval naturally throttles renders. It also notes that
  "render only on state changes" is possible but "adds complexity that is often unnecessary."
- The ratatui **Event Handling** concepts page and **FAQ** describe handler *architecture* and
  confirm the app owns the loop/redraw, but neither prescribes coalescing — the throttling
  guidance lives in the async templates above.

**Relevance to this repo:** those examples are async (`EventStream` + tokio); ADR-0005 keeps
this TUI **synchronous** (one `App` reducer, no async runtime). The portable synchronous
equivalents that fit the existing loop are (a) the `poll(Duration::ZERO)` drain-to-empty +
single repaint from Q1, and/or (b) a minimum-frame-interval throttle around `terminal.draw`.
Both live entirely in `runtime.rs` at the I/O boundary and need no `core` change. Choosing
between them (and any interaction with the existing `POLL_INTERVAL`/`needs_poll` running-backup
path) is the #3-fix decision — out of scope for this note.

## Facts the decision rests on (summary)

1. `event::poll(Duration::ZERO)` + `event::read()` is a supported non-blocking drain; a
   `while poll(ZERO) { read() }` loop can coalesce a buffered burst into one repaint. The
   `use-dev-tty` poll bug (#839) does **not** apply — that feature is off here.
2. On Linux plain terminals crossterm emits **`Press` only**; held keys are OS/terminal
   autorepeat re-sent as `Press` events. `KeyEventKind::Repeat` needs kitty-protocol
   enhancement flags (`REPORT_EVENT_TYPES` + `REPORT_ALL_KEYS_AS_ESCAPE_CODES`) which this app
   does not push. Consuming `Repeat` is moot today and, even with flags, does not reduce
   latency — coalescing/throttling is the only lever, and it is orthogonal to Press-vs-Repeat.
3. Prior art coalesces by **capping render rate** independent of input (async ratatui recipes:
   split tick/frame rate). The synchronous fits here are a poll(ZERO) drain and/or a
   min-frame-interval throttle, both confined to `runtime.rs`.

## Sources

- docs.rs — crossterm 0.29 `event::poll`
  (<https://docs.rs/crossterm/latest/crossterm/event/fn.poll.html>),
  `event::read` (<https://docs.rs/crossterm/latest/crossterm/event/fn.read.html>),
  `KeyEventKind` (<https://docs.rs/crossterm/latest/crossterm/event/enum.KeyEventKind.html>),
  `KeyboardEnhancementFlags`
  (<https://docs.rs/crossterm/latest/crossterm/event/struct.KeyboardEnhancementFlags.html>).
- crossterm GitHub — poll(Duration::ZERO)/`use-dev-tty` bug #839
  (<https://github.com/crossterm-rs/crossterm/issues/839>), key-press-fires-twice #797
  (<https://github.com/crossterm-rs/crossterm/issues/797>).
- ratatui GitHub — duplicate key events on Windows #347
  (<https://github.com/ratatui/ratatui/issues/347>).
- Ratatui website — Event Handling (<https://ratatui.rs/concepts/event-handling/>),
  FAQ (<https://ratatui.rs/faq/>), Terminal-and-EventHandler recipe
  (<https://ratatui.rs/recipes/apps/terminal-and-event-handler/>), Async Event Stream tutorial
  (<https://ratatui.rs/tutorials/counter-async-app/async-event-stream/>).
- `d-holguin/async-ratatui` — render-throttling / tick-vs-frame-rate prior art
  (<https://github.com/d-holguin/async-ratatui>).
- This repo — `crates/tui/src/runtime.rs` (redraw-on-event loop, `read_event` Press-only
  filter, `init_terminal` pushes no enhancement flags), `docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md`
  (synchronous TUI), and prior note `docs/research/2026-07-18-ratatui-foundation.md`.
