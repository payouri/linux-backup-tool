# Ratatui + crossterm foundation for the TUI migration

Research resolving ticket [#46](https://github.com/payouri/linux-backup-tool/issues/46).
Scope: vet `ratatui` + `crossterm` and their headless testing story before migrating
the `inquire`-based interactive prompts to a full-screen ratatui app.
Bias: healthy/well-maintained crates, testability under this repo's RunContext /
single-round-trip-seam constraints.

Versions and stats confirmed via the crates.io API and docs.rs on **2026-07-18**.

## Recommended crate set

| Concern | Crate | Version | One-line rationale | Health (downloads / last release) |
|---|---|---|---|---|
| Full-screen TUI framework | **`ratatui`** | 0.30.2 | The maintained successor to `tui-rs`; immediate-mode widgets + layout, backend-agnostic, ships a first-class `TestBackend` for headless assertions. | 38.5M total / 14.5M recent; last release 2026-06-19. Actively maintained by the ratatui org. |
| Terminal backend / input | **`crossterm`** | 0.29.0 | Cross-platform terminal control (raw mode, alt screen, key/resize events) that ratatui uses as its default backend; the only backend that is also cross-platform. | 160.4M total / 39.5M recent; last release 2025-04-05. Mature, stable cadence. |

**One-line dependency justification (for AGENTS.md):**
`ratatui` 0.30 is the actively-maintained fork of the abandoned `tui-rs` (blessed by the
original author, Feb 2023), is the de-facto standard Rust TUI framework (38M+ downloads),
and ships a built-in `TestBackend` for headless rendering assertions; chosen over `cursive`
(≈1.6M downloads, callback/event-loop model, far less traction) because its immediate-mode
render-from-state model fits our existing state and its `TestBackend` fits our
render-only test seam. `crossterm` 0.29 is ratatui's default, cross-platform backend and
supplies the key/resize event model — chosen over `termion`/`termwiz` for Windows/portability
and because ratatui already depends on it.

### Why over the alternatives

| Alternative | Version | Why not |
|---|---|---|
| `cursive` | 0.21.1 | Retained-mode, callback/event-loop TUI toolkit. Far less adoption (1.6M total / 278K recent, last release 2024-08-03) and a heavier, view-tree programming model that fights our render-from-`RunContext`-state approach. |
| `tui-rs` | 0.19.0 | **Abandoned** — this is the crate ratatui forked. Last release 2022-08-14, effectively unmaintained (6.9M total but only 325K recent). Do not use; ratatui is the drop-in successor. |
| `termion` / `termwiz` | — | Alternative ratatui backends. `termion` is Unix-only (no Windows); `termwiz` is heavier (wezterm's terminal lib). `crossterm` is the default and only cross-platform option. |

**Net dependency line (Cargo.toml):**

```toml
ratatui   = "0.30"   # pulls crossterm 0.29 as its default backend
crossterm = "0.29"   # depend directly for the event/input model (KeyEvent, KeyModifiers)
```

## Versions, cadence, MSRV/edition

- **ratatui 0.30.2** (2026-06-19). Edition **2024**, MSRV **1.88.0** as of 0.30.1/0.30.2
  (0.30.0 was 1.86.0). Cadence: frequent minor/patch releases (0.30.0 2025-12-26 →
  0.30.1 2026-06-05 → 0.30.2 2026-06-19), roughly a minor line every few months with
  patches in between. Forked from `tui-rs` **February 2023**, "with the blessing of the
  original author, Florian Dehau."
- **crossterm 0.29.0** (2025-04-05). Edition **2021**, MSRV **1.63.0** (stable since 0.27).
  Slower, more mature cadence (0.27 2023-08 → 0.28 2024-07 → 0.29 2025-04) — a sign of
  stability, not neglect.

> **Edition note:** ratatui 0.30 requires Rust **1.88+** and is edition-2024. Confirm the
> workspace toolchain/`rust-version` clears 1.88 before pinning; crossterm's lower MSRV
> (1.63) is not the binding constraint.

## Testing story — headless rendering with `TestBackend`

ratatui ships `ratatui::backend::TestBackend`, an in-memory `Backend` implementation that
renders into a `Buffer` instead of a real terminal — deterministic, no TTY required, ideal
for the `tui` crate's tests.

Minimal example:

```rust
use ratatui::{Terminal, backend::TestBackend};

let backend = TestBackend::new(20, 3);          // width, height in cells
let mut terminal = Terminal::new(backend)?;

terminal.draw(|frame| render_app(frame, &state))?; // your pure render fn, fed injected state

// Assert on the rendered cells:
terminal.backend().assert_buffer_lines([
    "┌──────────────────┐",
    "│ Hello            │",
    "└──────────────────┘",
]);
```

Key API surface:

- `TestBackend::new(width: u16, height: u16)` — construct with fixed dimensions.
- `TestBackend::with_lines(lines)` — seed initial screen state, size inferred from the lines.
- `backend().buffer() -> &Buffer` — the rendered cell grid, for custom assertions on
  content/style per cell.
- `backend().assert_buffer(&Buffer)` / `assert_buffer_lines(lines)` — panic with a diff if
  the rendered buffer doesn't match.
- `assert_cursor_position(...)`, `cursor_visible()`, plus `assert_scrollback[_lines]()`.

**What the `tui` crate can realistically assert, and limits:**

- TestBackend verifies **rendered output** — that a given application state produces the
  expected on-screen cells/styles. This aligns with this repo's rules: keep `core` free of
  ambient reads (feed the render function state derived from the injected `RunContext`), and
  don't add new seams — `tui` render tests are a `tui`-crate concern, not a second `core`
  round-trip seam. The one `core` round-trip seam in `crates/core/tests/roundtrip.rs` stays
  the sole `core` seam; TUI rendering is asserted separately in `tui`.
- **Input events are NOT simulated by TestBackend.** It has no facility to inject key
  presses. To test event handling, factor the update/reducer logic into a pure function
  (`fn update(state, Event) -> state`) and unit-test it by constructing
  `crossterm::event::KeyEvent` values directly, then render the resulting state through
  TestBackend. Keep the real `crossterm::event::read()` loop thin and at the I/O boundary so
  it needs no automated coverage.

## Event / input model (crossterm)

Events come from `crossterm::event::read()` (blocking) or `poll(Duration)` + `read()`
(non-blocking); `EventStream` is the async alternative. `read()`/`poll()` must not be called
concurrently from multiple threads.

`Event` variants: `Key(KeyEvent)`, `Mouse(MouseEvent)`, `Resize(u16, u16)`,
`FocusGained`/`FocusLost`, `Paste(String)` (bracketed paste, feature-gated).

`KeyEvent { code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind, state: KeyEventState }`.

- `KeyCode` — `Char(char)`, `Enter`, `Esc`, `Tab`, `BackTab`, `F(u8)`, arrows, etc.
- `KeyModifiers` — bitflags: `CONTROL`, `ALT`, `SHIFT`, `SUPER`, `HYPER`, `META`. Test with
  `.contains(...)`.
- `KeyEventKind` — `Press`, `Repeat`, `Release`. **`Release`/`Repeat` are only reported on
  terminals supporting the kitty keyboard protocol**; on ordinary terminals you get `Press`
  only — filter on `kind == Press` to avoid double-handling where release *is* reported.

Detecting a Ctrl chord:

```rust
use crossterm::event::{KeyCode, KeyModifiers};
if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { /* ... */ }
```

### Critical caveats

- **Raw mode is required** for per-keystroke events (`terminal::enable_raw_mode()`); without
  it the terminal line-buffers input. ratatui's `init()` enables it for you.
- **Alternate screen**: enter with `EnterAlternateScreen` / leave with `LeaveAlternateScreen`
  so the user's scrollback is preserved. ratatui's `init()`/`restore()` (and the all-in-one
  `run()`) handle enter/raw-on and leave/raw-off.
- **Terminal restoration on panic**: a panic while in raw mode + alt screen leaves the
  user's terminal wrecked. `ratatui::run()` installs a panic hook that restores the terminal
  automatically; if using `init()`/`restore()` manually, install a panic hook that calls
  `restore()` (ratatui also exposes a panic-hook helper). Do **not** rely on `restore()` at
  the end of `main` alone.
- **Resize**: handle `Event::Resize(w, h)` and re-render; ratatui also re-computes layout on
  the next `draw`, but you must pump the event so the loop wakes.
- **Reserved / ambiguous chords** — do not bind these:
  - `Ctrl-C` (SIGINT), `Ctrl-Z` (SIGTSTP), `Ctrl-S`/`Ctrl-Q` (XON/XOFF flow control),
    `Ctrl-\` (SIGQUIT). In raw mode `Ctrl-C` arrives as a `KeyEvent`
    (`Char('c')` + `CONTROL`) rather than raising SIGINT, so you must handle quit yourself —
    but users still *expect* Ctrl-C to quit, so honor it.
  - `Ctrl-M` is indistinguishable from **Enter** (both `KeyCode::Enter`), and `Ctrl-I` from
    **Tab** — a terminal/ASCII limitation, not a crossterm bug. Likewise `Ctrl-[` == Esc.
    Avoid binding these as distinct chords on plain terminals.
  - The **kitty keyboard protocol** (`PushKeyboardEnhancementFlags`) disambiguates the above
    (Ctrl-M vs Enter, key release events, more modifier combos) **only on supporting
    terminals** (kitty, foot, recent WezTerm/Ghostty). Treat it as a progressive
    enhancement; the safe baseline is: Press-only, no Ctrl-M/Ctrl-I/Ctrl-[ distinction.

## Recommendation

Pin `ratatui = "0.30"` and `crossterm = "0.29"`. Confirm the workspace toolchain is
**Rust ≥ 1.88** (ratatui 0.30 is edition-2024, MSRV 1.88); this is the only edition/MSRV
gate. Use `ratatui::run()` (or `init()`/`restore()` + an explicit panic hook) for terminal
lifecycle. Structure the app as a pure `update(state, Event) -> state` + a pure render
function so event logic is unit-testable via constructed `KeyEvent`s and rendering is
assertable via `TestBackend` — with no new `core` seam and no ambient reads.

## Sources

- crates.io API (`/api/v1/crates/<name>` and `/versions`), fetched 2026-07-18 — download
  counts, versions, edition, `rust_version` (MSRV) for ratatui, crossterm, cursive, tui.
- ratatui FAQ — <https://ratatui.rs/faq/> (tui-rs fork history/blessing, backends).
- docs.rs — `ratatui` 0.30 `backend::TestBackend`
  (<https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html>) and crate root
  `init()`/`restore()`/`run()` (<https://docs.rs/ratatui/latest/ratatui/index.html>).
- docs.rs — `crossterm` 0.29 `event` module
  (<https://docs.rs/crossterm/latest/crossterm/event/index.html>),
  `event::KeyModifiers` (<https://docs.rs/crossterm/latest/crossterm/event/struct.KeyModifiers.html>).
- crossterm issues on Ctrl-M vs Enter (#633) and Ctrl-C in raw mode / key-down vs release
  (#642, #214) — <https://github.com/crossterm-rs/crossterm/issues/633>.
