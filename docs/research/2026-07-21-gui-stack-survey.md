# GUI stack candidate survey: Tauri / egui / iced / Dioxus-desktop

Research resolving ticket [#55](https://github.com/payouri/linux-backup-tool/issues/55) (part of
[#54](https://github.com/payouri/linux-backup-tool/issues/54)).
Scope: gather the facts a future **GUI** entry point — a second front end beside the ratatui TUI,
both consuming the *synchronous* Rust `core` library — would rest on, for four candidates:
**Tauri** (React/Tailwind/shadcn/TanStack Query front end), **egui**, **iced**, and
**Dioxus-desktop**.
Bias: high-trust primary sources (official docs, GitHub, release notes, crates.io); the same
health/maintainability lens the ratatui-foundation note used.

**This note gathers facts only. The stack *decision* is ticket
[#56](https://github.com/payouri/linux-backup-tool/issues/56) (T1) — no recommendation is made here.**

Versions and stats confirmed via the crates.io API on **2026-07-21**; other claims cited inline.

## Constraints this survey measures each candidate against

Pulled from the ADRs so the facts below are scored against *our* boundary, not a generic one:

- **`core` is synchronous, `#![forbid(unsafe_code)]`, no runtime.** ADR-0005 D4 explicitly
  rejects `tokio`/async for the one background job and drives progress over a plain
  `std::sync::mpsc` channel. A GUI framework that *forces* an async runtime into the app is a
  friction point to note (not necessarily a disqualifier).
- **Rust owns authoritative state; the UI reflects it.** ADR-0005 D1: `core::SelectionModel` is
  owned by the app and mutated only through its public API; view state lives *around* it. The
  question per candidate is whether the model can stay Rust-owned and authoritative.
- **The file-tree/explorer is the hard UI.** ADR-0006 makes recursive drill-down the central
  interaction: a navigable directory tree derived from a flat `Vec<Entry>` (can be 10k+ nodes),
  with subtree/exact deselection, tri-state (`All`/`Partial`/`None`) directory badges, multi-select
  and keyboard nav. The UX ceiling for a tree/explorer is therefore the load-bearing axis.
- **`core`/UI split is compiler-enforced** (ADR-0001): a GUI slots in as a new crate beside `tui`
  without touching `core`. All four candidates preserve this — none require changes to `core`.

## Health / maturity (crates.io, 2026-07-21)

| Candidate | Latest stable | 1.0? | Edition / MSRV | Downloads (total / recent 90d) | Cadence |
|---|---|---|---|---|---|
| **Tauri** | **2.11.5** (2026-07-01) | **Yes — 2.0 GA 2024-10-02** ([blog](https://v2.tauri.app/blog/tauri-20/)) | 2021 / **1.77.2** | 22.6M / 8.3M | Frequent patches on a stable 2.x line |
| **egui** | **0.35.0** (2026-06-25) | **No** (0.x, but very mature) | 2024 / **1.92** | 20.1M / 4.5M | Regular minor line every ~1–2 months |
| **iced** | **0.14.0** (2025-12-07) | **No** (0.x) | 2024 / **1.88** | 2.35M / 0.60M | Slow, large releases (0.13 2024-09 → 0.14 2025-12) |
| **Dioxus-desktop** | **0.7.9** (2026-05-08) | **No** (0.x; 0.8.0-alpha.0 in flight) | 2021 / **1.83.0** | 0.85M / 0.26M | Active; frequent 0.7.x patches |

Notes:

- **Tauri** is the only candidate past 1.0 — a stable, semver-guaranteed 2.x API since Oct 2024
  ([Tauri 2.0 Stable Release](https://v2.tauri.app/blog/tauri-20/); [Wikipedia](https://en.wikipedia.org/wiki/Tauri_(software_framework))).
  Its low MSRV (1.77.2) is well under our current `rust-version = "1.95"`.
- **egui** is 0.x but by far the most-downloaded pure-Rust option and on a steady release
  treadmill; its 0.34/0.35 line requires **Rust 1.92** (edition 2024) — clears our 1.95 toolchain.
- **iced** ships large, infrequent releases; 0.14 (Dec 2025) was the first release in ~15 months
  after 0.13. Lower absolute adoption than egui/Tauri.
- **Dioxus** is pre-1.0 and mid-transition (0.8 alpha). Lowest adoption of the four; `dx bundle`
  Linux packaging has open reliability issues (see Packaging below).

## Candidate 1 — Tauri (React / Tailwind / shadcn / TanStack Query front end)

**Consuming a synchronous core.** Tauri splits the app into a Rust backend and a JS/web front end
in a system WebView. Rust functions annotated `#[tauri::command]` are registered via
`tauri::generate_handler![]` and called from JS through `invoke()`
([Calling Rust from the Frontend](https://v2.tauri.app/develop/calling-rust/)). Core stays fully
synchronous: a command can be an ordinary sync `fn`. **However, the entire cross-boundary contract
is `serde`:** "all command arguments and return values must implement `serde::Deserialize` /
`serde::Serialize`" — *including errors* (hence the `thiserror`-with-`Serialize` pattern). Our
`core` errors already use `thiserror` (ADR-0005 D7) but would need `Serialize` derives at the
GUI boundary.

- **State ownership.** Authoritative state stays in Rust via **managed state**: `.manage(x)` stores
  a value and commands receive it as `tauri::State<T>` — runtime dependency injection
  ([Calling Rust](https://v2.tauri.app/develop/calling-rust/)). So `core::SelectionModel` can live
  Rust-side (behind a `Mutex`/`RwLock` in managed state) with React holding only a *serialized
  reflection* of it. Caveat: this means the model is **mirrored** across the JS↔Rust boundary as
  JSON, not directly bound — every mutation round-trips through `invoke` + a re-serialized snapshot.
  Borrowed types (`&str`, `State<'_, T>`) cannot appear in `async` command signatures.
- **Async pressure.** Tauri commands can be sync; async commands run on a task pool. Tauri does run
  its own async event loop internally, but it does **not force our app logic to be async** — a
  sync core called from sync commands is idiomatic.

**File-tree / explorer ceiling.** This is Tauri's strongest axis, via the *JS* ecosystem rather
than Rust: **react-arborist** (v3.15.0, [npm](https://www.npmjs.com/package/react-arborist)) is a
purpose-built tree component with **virtualization** (via `react-window`), **drag-and-drop**
(`react-dnd`), **multi-selection**, filtering, and keyboard navigation off the shelf — i.e. exactly
the ADR-0006 interaction, pre-built. **TanStack Virtual** provides headless list/grid
virtualization for 10k+ rows if a custom tree is built instead, and **shadcn/ui** supplies the
component styling layer. Tri-state directory badges and the `directory_state` mapping would be
front-end presentation over data returned by a core-backed command.

**Linux packaging.** Ship formats: **Debian (.deb), RPM, AppImage, Snap, Flatpak, AUR**
([Distribute overview](https://v2.tauri.app/distribute/)). The defining trait: Tauri renders in the
**OS-native WebView, WebKitGTK 4.1** on Linux, so the shipped app is **not** self-contained.

- Build-time prereqs include `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, plus `libxdo`, `librsvg2`,
  `libayatana-appindicator3` ([Prerequisites](https://tauri.app/start/prerequisites/)).
- The `.deb` bundler declares runtime deps `libwebkit2gtk-4.1-0` and `libgtk-3-0`
  ([Debian distribute guide](https://v2.tauri.app/distribute/debian/)), so **WebKitGTK 4.1 must be
  present on the target** (Ubuntu 22.04 / Debian 12 baseline). AppImage bundles the deps in and is
  correspondingly larger; `.deb`/`.rpm` rely on the system WebKitGTK.
- Binary itself is small (native web renderer, not bundled Chromium) — a key selling point vs
  Electron — but the **WebKitGTK runtime dependency** is the packaging cost to weigh vs the
  near-static pure-Rust binaries below.

**Long-job / progress story.** Two backend→frontend mechanisms
([Calling Frontend](https://v2.tauri.app/develop/calling-frontend/)): the **event system**
(`app.emit("evt", payload)` / JS `listen()`) for lower-frequency, multi-consumer notifications, and
**channels** (`tauri::ipc::Channel<T>`, `.send()`) which are "designed to be fast and deliver
ordered data" — used internally for download progress and child-process output, i.e. the natural fit
for streaming backup/restore progress. Our existing ADR-0005 `ChannelProgress` (mpsc) would bridge
into a Tauri `Channel` or `emit` at the command boundary; the background job still runs on a
`std::thread`. Payloads must be `serde`-serializable.

## Candidate 2 — egui

**Consuming a synchronous core.** egui is a pure-Rust **immediate-mode** GUI: the whole UI is
re-laid-out and repainted each frame ([docs.rs/egui](https://docs.rs/egui/latest/egui/)). It is
**synchronous and does not require async/tokio** — the docs state the app owns its data and passes
egui `&mut` references each frame; egui "doesn't store your data." This is the tightest fit for
ADR-0005 D1: `core::SelectionModel` can be a plain field the render code reads and mutates directly
each frame — **no serialization, no boundary, no mirror.** (eframe is the usual windowing/event-loop
integration layer around egui.)

**File-tree / explorer ceiling.** Weakest off-the-shelf tree story. egui ships container widgets —
`ScrollArea`, `CollapsingHeader`, `Grid`, panels
([docs.rs/egui](https://docs.rs/egui/latest/egui/)) — but **no first-class tree/explorer widget**: a
directory tree is hand-rolled from nested `CollapsingHeader`s. Virtualization exists but is
**row-oriented, not tree-oriented**: `ScrollArea::show_rows(row_height, total_rows, ...)` "efficiently
show[s] only the visible part of a large number of rows" and `show_viewport` paints only the visible
rectangle ([ScrollArea docs](https://docs.rs/egui/0.35.0/egui/containers/scroll_area/struct.ScrollArea.html)).
To virtualize 10k+ nodes you must **flatten the visible/expanded tree into a row list yourself**
and feed `show_rows`. Multi-select, drag-select, tri-state badges, and keyboard nav are all
hand-built. Ceiling is high (immediate mode makes custom interaction cheap) but the **labour is
ours** — closest in spirit to the ratatui tree we're already building.

**Linux packaging.** Pure-Rust; no WebView. Produces a **near-static native binary** (links system
graphics/windowing libs via `winit`/`glow`/`wgpu`, but no WebKitGTK/GTK web stack). No official
one-command bundler comparable to `tauri`/`dx`: shipping `.deb`/AppImage/Flatpak is done with
generic Rust tooling (`cargo-deb`, `cargo-bundle`, linuxdeploy) rather than a first-party pipeline.
Bundle is small and self-contained modulo standard system libs.

**Long-job / progress story.** Immediate mode + a background thread: the worker calls
`egui::Context::request_repaint()` to wake the UI when new progress arrives
([docs.rs/egui](https://docs.rs/egui/latest/egui/)); the render loop reads the shared progress state
(e.g. an `Arc<Mutex<..>>` or the receiving end of our mpsc channel via `try_recv`) each frame. This
maps almost exactly onto ADR-0005 D4 — `std::thread` worker + channel, no async — with
`request_repaint()` substituting for the crossterm poll wakeup.

## Candidate 3 — iced

**Consuming a synchronous core.** iced is a pure-Rust framework built on **The Elm Architecture**:
`State`, a `Message` enum, and pure `update(&mut state, Message)` / `view(&state)` functions
([docs.rs/iced](https://docs.rs/iced/latest/iced/)). State is **Rust-owned** — "you can define your
own struct for your state" with full ownership, no forced heap/shared refs — so `core::SelectionModel`
lives directly in the app `State` and is mutated in `update`. This matches our reducer shape well
(it is essentially the same `update(state, Event) -> state` model the ratatui app already uses per
ADR-0005 D1).

- **Async pressure.** iced does **not force async** for basic apps, but concurrency is expressed
  through its own async primitives: `update` may *optionally* return a **`Task`** (runs a future or
  stream, results delivered back as a `Message`), and **`Subscription`**s are declarative long-lived
  streams (docs). These run on iced's own executor — you don't pull in `tokio` yourself, but the
  idiomatic way to drive background work *is* the async `Task`/`Subscription` API, which is a
  different shape from our sync-thread-+-mpsc model (ADR-0005 D4). Bridging our sync worker to a
  `Subscription` is possible but is more framework-idiomatic-async than egui/ratatui.

**File-tree / explorer ceiling.** No built-in tree/explorer widget and **no list virtualization**.
Built-in widgets are `Column`/`Row`/`Container`/`Scrollable` etc. ([widget index](https://docs.rs/iced/latest/iced/widget/index.html));
a tree is hand-rolled. Two easy-to-confuse names: `iced::widget::Lazy`
([docs](https://docs.rs/iced/latest/iced/widget/struct.Lazy.html)) is **view memoization**, not
virtualization; and `iced::advanced::widget::Tree` is iced's *internal widget-state tree*, **not** a
UI file-tree widget. So for 10k+ nodes you both hand-roll the tree *and* implement virtualization
(a custom widget via the `advanced::Widget` trait). Highest hand-rolling cost of the four for the
core interaction.

**Linux packaging.** Pure-Rust, no WebView, near-static native binary (same class as egui). No
first-party bundler; ship via generic Rust packaging tools. Self-contained modulo standard system
graphics libs.

**Long-job / progress story.** The idiomatic path is a **`Subscription`** streaming progress
`Message`s into `update`, or a `Task` running the job and reporting completion (docs). A background
backup would be modelled as a subscription/stream feeding progress messages — clean within TEA, but
async-flavoured rather than the sync-channel approach ADR-0005 chose for the TUI.

## Candidate 4 — Dioxus-desktop

**Consuming a synchronous core.** Dioxus is a React-like (RSX/component/hooks) pure-Rust framework.
Crucially, **`dioxus-desktop` renders in a system WebView via `wry`** — WebKitGTK on Linux, WebView2
on Windows, WebKit on macOS ([Dioxus desktop guide](https://dioxuslabs.com/learn/0.7/guides/platforms/desktop/);
[dioxus-desktop crate](https://crates.io/crates/dioxus-desktop); wry is the same webview layer Tauri
uses). Unlike Tauri, though, your Rust code and the UI components are **both Rust in one process** —
there is no JS front end and no `serde` command boundary; state is shared Rust values via hooks/signals,
so `core::SelectionModel` can be held in Rust signals and read by components directly (closer to
egui/iced on the state-ownership axis, while sharing Tauri's WebKitGTK rendering dependency).

- **Async pressure.** Dioxus has a **built-in async runtime and hooks** (`use_future`,
  `use_coroutine`, `spawn`); async is first-class and idiomatic for side effects, though your core
  calls can remain sync (run a blocking job on a thread and feed a signal). Reactivity is
  signal-based, so state stays Rust-owned and authoritative.

**File-tree / explorer ceiling.** Because rendering is HTML/CSS in a webview, the tree can be built
with web techniques, but Dioxus's **JS component ecosystem is not directly reusable** (react-arborist
is a React component; Dioxus is not React) — you'd hand-roll the tree in RSX or wrap a JS lib via
interop. Virtualization would likewise be hand-built (or via a JS interop lib). Middle ground: web
layout primitives help, but no drop-in Rust tree widget and no direct access to Tauri's React tree
components.

**Linux packaging.** `dx bundle` produces desktop bundles including **AppImage and `.deb`**
(`dx bundle --desktop --package-types deb`) ([bundle tutorial](https://dioxuslabs.com/learn/0.7/tutorial/bundle/)).
Because it's webview-based (`wry`), the shipped Linux app **depends on WebKitGTK at runtime just like
Tauri** — it is not a self-contained pure-Rust binary despite Dioxus being pure-Rust source. Note
several **open Linux bundling reliability issues** (broken AppImage/`.deb`, missing icons, CSS not
loading in some modes — [#3689](https://github.com/DioxusLabs/dioxus/issues/3689),
[#3426](https://github.com/DioxusLabs/dioxus/issues/3426),
[#3111](https://github.com/DioxusLabs/dioxus/issues/3111)).

**Long-job / progress story.** A background job runs on a thread (or via `spawn`/`use_coroutine`),
pushing progress into a **signal**; components re-render reactively when the signal updates. Our
existing mpsc `ChannelProgress` can feed a coroutine that writes the signal. Async-capable but does
not force async onto core.

## Comparison table

| Axis | Tauri | egui | iced | Dioxus-desktop |
|---|---|---|---|---|
| **Latest / 1.0?** | 2.11.5 · **1.0 (2.0 GA 2024-10)** | 0.35.0 · 0.x | 0.14.0 · 0.x | 0.7.9 · 0.x (0.8-alpha) |
| **Adoption (recent 90d dl)** | 8.3M (highest) | 4.5M | 0.60M | 0.26M (lowest) |
| **MSRV / edition** | 1.77.2 / 2021 | 1.92 / 2024 | 1.88 / 2024 | 1.83 / 2021 |
| **Rendering** | System WebView (WebKitGTK) | Pure-Rust immediate mode | Pure-Rust (TEA, retained) | System WebView via `wry` (WebKitGTK) |
| **State stays Rust-authoritative** | Yes, but **mirrored as JSON** over IPC | Yes, **direct `&mut` each frame** | Yes, **owned in TEA `State`** | Yes, **Rust signals** |
| **serde boundary required** | **Yes** (all args/returns/errors) | No | No | No |
| **Forces async runtime on app** | No (sync commands OK; Tauri runs its own loop) | **No** | No, but idiomatic work is async `Task`/`Subscription` | Has built-in async runtime; core calls can stay sync |
| **Off-the-shelf tree widget** | **Yes — react-arborist** (virtualized, DnD, multi-select, kbd) | No — hand-roll from `CollapsingHeader` | No — hand-roll (custom widget) | No drop-in (JS libs not directly reusable) |
| **10k+ node virtualization** | Yes (react-window / TanStack Virtual) | Yes but **row-based** (`ScrollArea::show_rows`; flatten tree yourself) | **None built-in** (hand-roll) | Hand-roll / JS interop |
| **Linux ship formats** | deb/rpm/AppImage/Snap/Flatpak/AUR | Generic tooling (cargo-deb/bundle) | Generic tooling | `dx bundle` (AppImage/deb) — open reliability issues |
| **Runtime dep** | **WebKitGTK 4.1 + GTK3** required | Near-static native binary | Near-static native binary | **WebKitGTK** required (via wry) |
| **Progress mechanism** | events / `ipc::Channel` (serde payloads) | `Context::request_repaint()` + shared state | `Subscription`/`Task` streaming messages | signal updated from thread/coroutine |
| **Fit with ADR-0005 D4 (thread+mpsc, no async)** | Bridge mpsc → emit/Channel | **Direct** (thread + channel + repaint) | Bridge mpsc → Subscription (async-ish) | Bridge mpsc → coroutine/signal |

## Open questions for the decision (#56)

Facts above surface these tensions for T1 to weigh — this note does **not** resolve them:

1. **Tree UX vs runtime dependency.** Tauri is the *only* candidate with an off-the-shelf
   virtualized, multi-select, keyboard-navigable tree (react-arborist) — the exact ADR-0006
   interaction, pre-built — but it (and Dioxus-desktop) drags in a **WebKitGTK 4.1 runtime
   dependency**, while egui/iced ship near-static binaries but require **hand-rolling** the tree and
   its virtualization. How is "pre-built tree" weighed against "self-contained binary"?
2. **The serde mirror boundary (Tauri only).** ADR-0005 D1 wants `SelectionModel` Rust-authoritative;
   Tauri keeps it Rust-side via managed state but forces every read/mutation to cross a JSON IPC
   boundary and needs `Serialize` on core error types. Is a *mirrored* model acceptable, or is the
   direct `&mut`/signal ownership of egui/iced/Dioxus preferable in principle?
3. **Async idiom vs the deliberate sync-thread choice.** ADR-0005 D4 chose thread+mpsc and explicitly
   rejected async for one background job. egui maps onto that directly; iced's idiomatic path is
   `Subscription`/`Task` (async-flavoured); Dioxus has a built-in async runtime. Does re-introducing
   async at the GUI layer contradict the spirit of D4, or is it acceptable in a different front end?
4. **Maturity/stability appetite.** Only Tauri is post-1.0. egui is 0.x but heavily adopted and
   steady; iced ships rarely; Dioxus is pre-1.0, lowest-adoption, with **open Linux bundling bugs**.
   How much does 1.0/stability and Linux-packaging reliability weigh?
5. **Front-end skill surface & code sharing.** Tauri means a full JS/React/Tailwind/shadcn/TanStack
   stack alongside Rust (two languages, two ecosystems); egui/iced/Dioxus keep everything in Rust and
   could share types directly with `core`. What is the team's appetite for the JS toolchain?
6. **MSRV.** egui 0.35 needs Rust 1.92 and iced 0.14 needs 1.88; the workspace is on 1.95, so all
   clear today — but egui's fast-moving MSRV is the tightest to track going forward.

## Sources

- crates.io API (`/api/v1/crates/<name>` and `/versions`), fetched 2026-07-21 — versions, download
  counts, edition, MSRV for tauri, egui, iced, dioxus, dioxus-desktop.
- Tauri — [Prerequisites](https://tauri.app/start/prerequisites/) (WebKitGTK 4.1 build deps),
  [Calling Rust from the Frontend](https://v2.tauri.app/develop/calling-rust/) (commands/IPC, serde,
  managed `State`), [Calling the Frontend from Rust](https://v2.tauri.app/develop/calling-frontend/)
  (events, `ipc::Channel`), [Distribute overview](https://v2.tauri.app/distribute/) and
  [Debian guide](https://v2.tauri.app/distribute/debian/) (ship formats, runtime deps),
  [Tauri 2.0 Stable Release](https://v2.tauri.app/blog/tauri-20/) and
  [Wikipedia: Tauri](https://en.wikipedia.org/wiki/Tauri_(software_framework)) (1.0/2.0 GA dates).
- egui — [docs.rs/egui](https://docs.rs/egui/latest/egui/) (immediate mode, state ownership,
  `request_repaint`, MSRV 1.92) and
  [ScrollArea](https://docs.rs/egui/0.35.0/egui/containers/scroll_area/struct.ScrollArea.html)
  (`show_rows`/`show_viewport` row virtualization).
- iced — [docs.rs/iced](https://docs.rs/iced/latest/iced/) (TEA, `Task`/`Subscription`, state
  ownership), [widget index](https://docs.rs/iced/latest/iced/widget/index.html),
  [`Lazy`](https://docs.rs/iced/latest/iced/widget/struct.Lazy.html) (memoization, not virtualization).
- Dioxus — [desktop platform guide](https://dioxuslabs.com/learn/0.7/guides/platforms/desktop/) and
  [dioxus-desktop crate](https://crates.io/crates/dioxus-desktop) (wry/WebKitGTK rendering),
  [bundle tutorial](https://dioxuslabs.com/learn/0.7/tutorial/bundle/) (`dx bundle`), open Linux
  bundling issues [#3689](https://github.com/DioxusLabs/dioxus/issues/3689),
  [#3426](https://github.com/DioxusLabs/dioxus/issues/3426),
  [#3111](https://github.com/DioxusLabs/dioxus/issues/3111).
- JS tree ecosystem (for Tauri) — [react-arborist (npm)](https://www.npmjs.com/package/react-arborist)
  (v3.15.0; virtualization via react-window, DnD, multi-select, keyboard nav).
