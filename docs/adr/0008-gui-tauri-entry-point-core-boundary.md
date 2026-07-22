# The GUI is a Tauri app in `crates/gui`, consuming a synchronous `core` over a JSON IPC boundary

**Status:** Accepted · **Date:** 2026-07-22

A second desktop entry point — a GUI beside the ratatui TUI, offering explorer/tree-style file
selection — is the [ADR-0001](0001-core-tui-split-no-dedup-trait.md) "future `gui`" that the ratatui
overhaul map ([#45](https://github.com/payouri/linux-backup-tool/issues/45)) explicitly ruled out of
its own scope as "a separate effort." This ADR is that effort's destination: it fixes the **stack and
high-level architecture** — the GUI is built with **Tauri**, consuming `core` via a **JSON IPC
boundary** — and nothing below it. It resolves the wayfinder map
[#54](https://github.com/payouri/linux-backup-tool/issues/54) and synthesizes its four decision
tickets: the stack ([#56](https://github.com/payouri/linux-backup-tool/issues/56)), the core↔GUI
boundary ([#57](https://github.com/payouri/linux-backup-tool/issues/57)), the progress/run model
([#58](https://github.com/payouri/linux-backup-tool/issues/58)), and the crate layout + packaging
([#59](https://github.com/payouri/linux-backup-tool/issues/59)).

This is **decision-only**: the per-screen implementation spec — the IPC command catalog, the tree
component pick, error/empty states — is a successor effort, born from this ADR exactly as #45 was born
from [ADR-0005](0005-ratatui-app-architecture-core-tui-boundary.md). The GUI is **additive**: the
ratatui TUI stays; both are independent consumers of `core`.

The stack survey grounding Decision 1 is settled separately in
[`docs/research/2026-07-21-gui-stack-survey.md`](../research/2026-07-21-gui-stack-survey.md)
(the four candidates, all primary-sourced 2026-07-21).

## Decisions

### 1. The stack is Tauri 2.x, front end React + Tailwind + shadcn + TanStack Query

The GUI is a **Tauri 2.x** app: a Rust backend owning `core`, a web-tech front end in **React +
Tailwind + shadcn + TanStack Query**. The rank-1 tie-breaker, fixed by grilling and *not*
re-litigated here, is the driver's **velocity in a stack they already know** plus the **rich JS
component ecosystem**; a pure-Rust GUI would have had to win *decisively* on architectural simplicity
or packaging to overturn that prior, and none does.

Tauri is the only candidate shipping the headline interaction — the [ADR-0006](0006-recursive-drill-down-exclusion-model.md)
recursive drill-down tree — **pre-built**: `react-arborist` gives a virtualized, multi-select,
keyboard-navigable tree, with TanStack Virtual for 10k+ nodes and shadcn styling. It is also the only
post-1.0 candidate (2.0 GA 2024-10), and its MSRV (1.77) clears the workspace's 1.95. The pure-Rust
options make you hand-roll the tree and its virtualization — egui virtualizes rows only, iced neither;
**Dioxus** shares Tauri's WebKitGTK cost *and* carries open Linux-bundling bugs, so it is strictly
dominated.

Two costs are accepted, both ranked below velocity:

1. **A WebKitGTK 4.1 runtime dependency** — the shipped Linux app is not self-contained the way a
   near-static egui/iced binary would be. Decision 4 is how it ships.
2. **A JSON IPC boundary** — `core`'s selection state is mirrored as JSON across IPC (Decision 2), and
   errors are flattened to a serializable form (Decisions 2 and 3). This is the seam a pure-Rust GUI
   would not have.

### 2. `core::SelectionModel` stays authoritative; the IPC surface is a thin mutation mirror with lazy views

The state of record is `core::SelectionModel`, owned in the Tauri backend as managed `tauri::State`
— exactly as the ratatui `App` owns it ([ADR-0005](0005-ratatui-app-architecture-core-tui-boundary.md)
Decision 1). The React front end holds only derived/view state and reflects core via **TanStack
Query**; **Zustand** is adopted **only if** a genuine client-only global need later appears. One
selection engine, two UIs.

- **Mutations in — a thin 1:1 mirror** of `SelectionModel`'s seven mutation methods. Each Tauri
  command locks the managed state, calls the method, and returns `Result<(), SerializableCoreError>`.
  The command layer carries **no** selection logic — the analogue of the TUI's key handlers — so new
  behavior lands in `core` and both UIs inherit it.
- **Sync convention — invalidate, don't return.** Mutations do not return derived state; they
  invalidate the relevant TanStack Query keys and the front end refetches what is mounted. This
  **overturns ADR-0006's "returns the new derived tree" phrasing** in favor of query invalidation,
  which is cleaner with a lazily-materialized tree.
- **Views out — lazy per-prefix.** A `["tree", prefix]` query returns a prefix's immediate children
  plus each child's `directory_state` (`All`/`Partial`/`None`, ADR-0006); a cheap
  `["selection-summary"]` returns counts, total size, `is_empty`, `encryption_required`, and
  warnings. No eager whole-tree snapshot ever crosses IPC, so every serialized payload stays bounded.
- **Serialization lives in `crates/gui`, not on `core` types.** `crates/gui` defines `Serialize`
  view DTOs (tree node, `directory_state`, selection summary) built from `SelectionModel`'s plain
  accessors via `From` — the direct analogue of ADR-0005's "the consumer builds the tree." Deriving
  `Serialize` on `core`'s own types was rejected because it would silently make `core`'s internal
  layout a published wire contract at exactly the boundary this decision defines.

### 3. One `async` command runs a synchronous `core`; progress streams over a typed channel; cancellation is a separate token

Backup and restore share **one** run model. `core` stays synchronous and `#![forbid(unsafe_code)]`;
`tokio` arrives *with* Tauri but never reaches `core`.

- **Invocation & terminal result.** An `async` Tauri `#[command]` runs the core call via
  `tauri::async_runtime::spawn_blocking(|| core::backup(...))` and `.await`s it. **The command's
  return value *is* the terminal result** — one run = one TanStack Query `useMutation`. Core's sync
  signature is untouched by the invocation model.
- **Progress transport — a channel, not the event bus.** A per-invocation, typed, ordered
  `tauri::ipc::Channel<T>` — the direct analogue of ADR-0005 Decision 4's per-run `mpsc`. The
  GUI-side `Progress` impl holds the channel and sends from `on_phase`/`on_entry`. The channel carries
  **progress notifications only**; the command return carries the terminal result. (Below-ADR note
  for the spec: coalesce/throttle `on_entry` emission so the bridge is not flooded.)
- **Cancellation — a cooperative `CancelToken`, its own parameter.** A new `CancelToken` (a newtype
  over `Arc<AtomicBool>`, std-only, `forbid(unsafe)`-clean) is passed as **its own parameter** to
  `backup()`/`restore()` — **not** in `RunContext`, since it is live control input, not an ambient
  run fact ([ADR-0004](0004-core-runcontext-injection.md)). The front end's cancel command flips the
  bool; core polls it at its checkpoints. **`Progress` stays strictly one-way** — ADR-0005 Decision
  4's "not a control hook" invariant is *preserved*: observation and control are separate channels.
  - **Backup: front-half only.** Polled in the `Phase::Scanning` scan/build loop; cancellation stops
    being honored at `on_phase(WritingArchive)` (encrypt → write archive → write manifest is the
    committed half). Because the backup dir is still empty at scan time, a cancel always precedes any
    on-disk bytes and routes through `backup()`'s existing cleanup — nothing left behind, zero new
    cleanup code. Internally `Err(CoreError::Cancelled)`.
  - **Restore: per-Entry in the unpack loop.** Restore has no cancel-safe front half (every unpacked
    file is a commit; ADR-0002 is best-effort, no rollback). Cancel stops unpacking further entries
    and returns the **partial `RestoreOutcome` plus a `cancelled` marker**, on the `Ok` side — the
    user sees exactly what landed. This mirrors the [ADR-0002](0002-backup-restore-failure-asymmetry.md)
    backup/restore asymmetry.
- **Terminal outcome & panic surfacing.** One boundary enum the front end matches exhaustively:
  `RunOutcome { Success(..) | Cancelled | Failed(SerializableCoreError) | Internal(String) }`. Both the
  backup `Cancelled` error and the restore `cancelled` marker map to **`Cancelled`**, rendered
  neutral — never a failure. Real `CoreError`s become **`Failed`**. A **worker panic** is caught at
  the `spawn_blocking` await (a `JoinError`; the webview does not unwind), becomes a distinct
  **`Internal`** variant with the panic message **logged host-side, never leaked raw** — the GUI
  analogue of ADR-0005's "failures are in-app state, not a stack dump." A host-side panic hook keeps
  `Internal` panics diagnosable.

### 4. `crates/gui` is a new workspace member over `core`; Flatpak is the canonical Linux package

- **`crates/gui`** is a new workspace member — the Tauri Rust backend, depending on **`core` only**.
  The React front end is **co-located** under it at `crates/gui/ui/`, the Rust backend at
  `crates/gui/src/`, with `tauri.conf.json` pointing `frontendDist`/`devUrl` at `ui/`. Everything GUI
  lives under one directory and keeps the repo's `crates/<name>` convention — we do **not** adopt
  Tauri's `src-tauri/` naming. This is the **target layout**; the member is added by the successor
  build effort, not now, so an empty crate cannot break the `cargo` gates today.
- **`gui` and `tui` share no code directly.** Both are independent consumers of `core`; anything
  UI-agnostic they both need lives in `core`. A `crates/ui-common` door is left open **if real
  duplication later appears** — not created now, per ADR-0001 (the abstraction waits for the second
  real implementation).
- **Flatpak is the canonical distributable.** `org.gnome.Platform` ships a pinned, sandboxed
  `webkit2gtk-4.1`, so the cross-distro WebKitGTK-version risk raised by the survey and the stack
  decision is **resolved by pinning to the runtime** rather than trusting the host's webkit.
  **AppImage is the fallback** for users who won't/can't use Flatpak; the host-webkit fragility
  survives only on that path. Cost accepted: a Flatpak manifest + `flatpak-builder` in CI.

### 5. ADR-0001 is reaffirmed; this is a new ADR, not an amendment

The GUI slots beside `tui` as the second consumer of `core` that ADR-0001 anticipated, so the
core/UI split holds as-is. Decisions 2–3 keep `core` synchronous, ambient-free (ADR-0004), and the
sole owner of the selection model and filesystem content I/O (ADR-0005) — the GUI is the direct
analogue of the TUI spine, not a change to it.

## Consequences

- **`core` changes the successor build effort must make, beyond #45's spec (#53):**
  - Both `backup()` and `restore()` gain a **`CancelToken`** parameter — **genuinely new** here.
  - `restore()` gains a **`Progress`** parameter — *not* new to the GUI; it is already the tracked
    ADR-0005 Decision 4 / #53 follow-up.
  - **Nothing else.** No `Serialize` derives and no other signature changes reach `core`: the plain
    accessors, `directory_state`, and the rich typed `CoreError` that #45's spec delivers are
    sufficient. Today only `Manifest`/`Profile`/`location` types derive `Serialize` in `core`;
    `Entry`/`EntryType`/`EntryRoot`/`SelectionModel`/`CoreError` do not, and per Decision 2 they never
    will.
- **New work living entirely in `crates/gui`:** the `Serialize` view DTOs + their `From`
  conversions; a single unified **`SerializableCoreError`** (a lossy flattening of `CoreError` —
  `PathBuf`→`String`, `io::Error`→message — via one `From<&CoreError>`, wrapping both the fallible
  commands' `Err` and `RunOutcome::Failed`); the thin per-mutation command wrappers; and the lazy
  `directory_state` / selection-summary query commands.
- **The per-screen GUI implementation spec is the successor effort** — the IPC command catalog, the
  tree component pick, error/empty states. This ADR decides the stack and spine; it does not build the
  GUI.
- A contributor will be tempted to push `async` down into `core` "to do the backup properly" or to
  derive `Serialize` on `core`'s types "to skip the DTOs." Don't — Decision 3 keeps `core` sync on
  purpose, and Decision 2 keeps `core`'s layout off the wire on purpose.
- Two facts deferred to build/spec era: the exact `org.gnome.Platform` version to pin, and the
  `on_entry` throttling policy. A dev-time `webkit2gtk-4.1` build dependency is a given on any dev box
  and in CI regardless of ship format; the `cargo tauri dev` loop runs against host webkit, not the
  Flatpak sandbox.
