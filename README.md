# linux-backup-tool

An interactive Rust tool for backing up and restoring a Linux user's environment,
offered through two front ends over one engine: a **terminal wizard** (`tui`) and a
**desktop GUI** (`gui`, Tauri).

Backs up program configs, SSH keys, KDE Plasma layouts, shell configs and plugins,
user directories, installed-package manifests, and user-supplied custom paths — with
smart defaults you confirm interactively, a chosen output location on any writable
drive, and a manifest that records what was backed up so it can later be restored.

> 🚧 Early development. Design and planning are tracked as GitHub issues via
> [`/wayfinder`](docs/agents/issue-tracker.md).

## How it works

Backup is a forward-only flow: it scans your machine, proposes a smart-default
selection you refine, insists on encryption when the selection contains anything
sensitive, lets you pick where the backup lands, and writes a single self-describing
backup directory. Restore reads that directory back — verifying its integrity,
re-rooting home-tagged files under the restoring user, and preserving ownership and
permissions.

```
WHAT ──▶ [encryption] ──▶ WHERE ──▶ REVIEW ──▶ RUN
scan     auto-insist       pick       confirm    tar│zstd│age
```

Both front ends walk this same flow over the one authoritative `core::SelectionModel`,
so they agree on what "selected" means for identical inputs. The terminal wizard is
right for headless/SSH boxes; the desktop GUI adds an explorer/tree view with real
multi-select for drilling into large directories. Neither surface holds selection
logic of its own — a behavior change in `core` shows up identically in both.

Each backup produces one directory named `linux-backup-<host>-<timestamp>/` containing
the compressed (optionally encrypted) archive and a `manifest.json` describing exactly
what was captured.

## Interfaces

### Terminal — `linux-backup`

```
usage: linux-backup backup
       linux-backup restore <dir> [--verify] [--overwrite]
       linux-backup select
       linux-backup where
```

| Command | What it does |
| --- | --- |
| `backup` | The full wizard: WHAT → [encryption] → WHERE → REVIEW → RUN. |
| `restore <dir>` | Restore a backup directory. `--verify` re-checks every file's checksum after restoring; `--overwrite` overwrites colliding targets non-interactively (otherwise a single "confirm once" prompt is shown). |
| `select` | The standalone WHAT screen — the interactive category/entry checklist, on its own. |
| `where` | The standalone WHERE screen — the interactive output-location picker, on its own. |

### Desktop — GUI

A windowed Tauri app presenting the same wizard as point-and-click screens: an
explorer/tree WHAT view with tri-state (All/Partial/None) directories and multi-select
drill-down, the encryption and WHERE steps, a live backup RUN screen (phase +
per-entry progress, front-half cancel), and a Restore screen (backup-dir pick,
encrypted detection, collision confirmation, partial/ownership/`reinstall.sh`
reporting). It ships on Linux as a **Flatpak** (canonical, app ID
`io.github.payouri.linux-backup`, with a pinned sandboxed WebKitGTK and a vendored Rust
toolchain) and an **AppImage** fallback — see
[`docs/gui-packaging.md`](docs/gui-packaging.md).
Run it from source with `cargo tauri dev` (see [Building](#building)).

## Architecture

A three-crate Cargo workspace with a compiler-enforced split
([ADR-0001](docs/adr/0001-core-tui-split-no-dedup-trait.md),
[ADR-0008](docs/adr/0008-gui-tauri-entry-point-core-boundary.md)): all domain logic
lives in `core`, which carries **zero UI dependencies**; `tui` and `gui` are
independent thin views on top, sharing no code with each other.

```
crates/
├── core/   linux-backup-core — UI-free domain library (#![forbid(unsafe_code)])
├── tui/    tui — ratatui full-screen terminal app → binary `linux-backup`
└── gui/    linux-backup-gui — Tauri desktop app (Rust backend + React frontend in ui/)
```

`core` reads **nothing** from the ambient system: every run fact (`home`, output
location, privilege, `host`, `user`, `uid`, `os`, `now`) arrives through an injected
`RunContext` ([ADR-0004](docs/adr/0004-core-runcontext-injection.md)). Each front end's
entry point fills that context from the real machine; tests fill it with fixed values,
which makes the round-trip seam fully deterministic.

### `core` — the domain library

| Module | Responsibility |
| --- | --- |
| `model` | `SelectionModel` — the authoritative model of what will be backed up: category defaults, precedence resolution, drill-down toggles, and derived state (`encryption_required`, `total_size`, `selected_paths`, `warnings`). |
| `catalog` | The ten category definitions (SSH, GPG, Cloud creds, KDE Plasma, Agents, Shell, Program configs, Package lists, User data, Custom paths), each with its curated allowlist and precedence resolution across overlaps. |
| `scan` | `walkdir`-based discovery of category entries on the current machine: precedence resolution, hide-empty computation, sizing, and the safety-net scan for credential-like files. |
| `crypto` | Passphrase *policy*: the auto-insist rule (sensitive selection ⇒ encryption required) and the live passphrase-strength checklist. Never performs encryption itself. |
| `archive` | The concrete `tar → zstd → age` pipeline (age outermost on disk), with an identity path for unencrypted backups. |
| `location` | Output-location enumeration: mounted drives via `sysinfo`, the `lsblk`/`udisksctl` delta for unmounted USB partitions, and the always-offered `$HOME` row. |
| `packages` | Installed-package capture across six managers (pacman, yay/AUR, flatpak, snap, cargo, npm globals); a missing manager is skipped, never a failure. Versions are informational only. |
| `manifest` | The `manifest.json` schema and (de)serialization: schema/tool version, run facts, selected entries, per-file SHA-256 map, top-level `archive_sha256`, vanished files, and per-manager package lists. |
| `restore` | Untar-to-path restore: pre-flight `archive_sha256` check, privilege-aware uid/gid handling, re-rooting home-tagged entries under the restoring user's `$HOME`, `reinstall.sh` generation, and an opt-in `--verify` checksum pass. |
| `profile` | Load/save/prune of `profile.toml` — the last selection and output location. Encryption state is deliberately never persisted. |
| `context` | `RunContext` — the one struct through which every run fact enters `core`. |
| `fs_probe` | The filesystem *content* probes a view would otherwise do itself (subtree size, directory existence, encrypted-backup detection, directory listing for path autocomplete) — pulled into `core` so front ends render what `core` returns instead of touching the disk ([ADR-0005](docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md) D6). |
| `cancel` | `CancelToken` — the cooperative cancellation signal a caller flips to stop an in-flight backup/restore at its next checkpoint. Passed separately from `RunContext` (a live control input, not a fixed run fact) so progress stays strictly one-way observation. |
| `progress` | A callback trait each front end implements to drive per-phase progress; `core` stays synchronous and any channel adapter lives in the view crate. |
| `error` | The typed `CoreError` surface (`thiserror`; `anyhow` is reserved for the front ends). |

Top-level `core::backup()` and `core::restore::restore()` are the public seams the
round-trip test drives. Backup **fails fast** — any error after the backup directory is
created deletes it, so a directory with a manifest in it is always complete. Restore is
**best-effort** — it continues past per-file failures and reports each one
([ADR-0002](docs/adr/0002-backup-restore-failure-asymmetry.md)).

### `tui` — the terminal app

A full-screen [`ratatui`](https://ratatui.rs/) app: one `App` struct holds all view state,
behavior is a pure `update(state, Event) -> state` reducer, and rendering is a pure
function of that state
([ADR-0005](docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md)). That split is
what makes event logic unit-testable with hand-built `KeyEvent`s and rendering assertable
through `ratatui::backend::TestBackend` — neither needs a real terminal. It owns only
route, cursor/scroll, and input-buffer state; every decision is delegated to `core`.

| Module | Responsibility |
| --- | --- |
| `main` | Binary entry point: parses argv into a `Command`, maps it to the `Screen` it opens on, builds the machine's `RunContext`, and hands an `App` to the runtime. |
| `cli` | Hand-rolled argv parsing for the small subcommand grammar (no `clap`). |
| `runtime` | The one place that touches the terminal: raw mode, alternate screen, a terminal-restoring panic hook, the crossterm read loop, resize handling, and teardown. |
| `app` | The `App` reducer and its pure render — the `Screen` route, the fixed header/footer frame, and the redraw-on-event loop. |
| `wizard` | The completion-frontier stepper over a persistent `WizardState` (WHAT → encryption → WHERE → REVIEW), with WHAT→encryption sensitivity handled by derivation ([ADR-0007](docs/adr/0007-wizard-navigation-cross-step-invalidation.md)). |
| `what_screen` | The WHAT step body: the two-pane recursive drill-down explorer (categories + single-directory column with breadcrumb), subtree toggles, and the custom-path add field. |
| `encryption_screen` | The encryption step body: the required-vs-offered passphrase surface, `F2` reveal, live match indicator, invalidation banner, and the plaintext second-confirmation modal. |
| `where_step` | The WHERE step body: the output-location pick-list plus a custom-path field with fish-style autocomplete. |
| `run_screen` | The RUN screen: `core::backup` on a worker thread, its `mpsc` progress bridge, and the polled widget state the render draws. |
| `restore_screen` | The restore route (overwrite-confirm → passphrase unlock → RUN → result), separate from the backup wizard, prompting for the passphrase *before* any file is touched. |
| `keymap` | The single catalog of every shortcut's spelling *and* canonical binding, so the footer, the help overlay, and the reserved-chord guard can't drift apart. |
| `help` | The `F1` overlay: the exhaustive keybinding reference, rendered at every terminal size. |
| `path_autocomplete` | The shared fish-style path-completion engine behind both custom-path fields (existing paths only, hidden dotfiles, inline ghost text). |
| `reflow` | The reflow ladder: how the frame sheds chrome as the terminal shrinks, down to a "terminal too small" notice. |
| `glyphs` | The no-emoji ASCII fallback glyph set, detected once at startup and threaded into the pure renders. |
| `system` | Builds a real `RunContext` from the actual machine (`$HOME`, hostname, uid/euid privilege, OS, wall clock). |

### `gui` — the desktop app

A [Tauri](https://tauri.app/) 2.x app depending on `core` only: a Rust backend under
`crates/gui/src/` and a co-located React + Tailwind + shadcn + TanStack Query frontend
under `crates/gui/ui/`. The backend owns the authoritative `SelectionModel` as managed
Tauri state and exposes it over a JSON IPC boundary
([ADR-0008](docs/adr/0008-gui-tauri-entry-point-core-boundary.md)); `core` stays
synchronous and gains no serde derives.

| Layer | Responsibility |
| --- | --- |
| `commands` | A thin 1:1 mirror of `SelectionModel`'s mutations plus lazy per-prefix `["tree", …]` / selection-summary queries. Mutations invalidate query keys rather than returning derived state; the command layer holds **no** selection logic. |
| `state` / `tree` / `run_context` | The managed `SelectionModel` state, the lazy per-prefix tree projection served to the explorer, and the real `RunContext` built at the GUI's boundary. |
| `encryption` | The GUI-side encryption bookkeeping over `core::crypto`'s pure auto-insist policy — the "which sensitive set did this plaintext decision cover" context that ADR-0007 keeps view-side, held independently from `tui`'s copy since the two front ends share no code. |
| `run` / `restore` | The async run model: `core::backup`/`restore` run via `spawn_blocking`, progress streams over a per-invocation typed channel, cancellation flows through `core`'s `CancelToken`, and restore bridges the collision confirmation as a request→response over IPC. |
| `dto` / `error` | `Serialize` view DTOs built from `core`'s plain accessors via `From`, and the single lossy `SerializableCoreError` — the only error type that crosses IPC. |
| `ui/` | The React screens (explorer tree, wizard, RUN, Restore) reflecting `core` state via TanStack Query; the tree uses a virtualized, multi-select, keyboard-navigable component for 10k+ nodes. |

## Building

Rust **1.95** or newer (the floor `sysinfo` already forces; CI's `msrv` job compiles the
workspace against exactly 1.95.0):

```sh
cargo build --release          # terminal binary at target/release/linux-backup
cargo run -p tui -- backup     # run the wizard from source
```

The desktop GUI is driven through the Tauri CLI from `crates/gui/` (a dev-time
`webkit2gtk-4.1` build dependency and Node.js 24 — see `crates/gui/ui/.nvmrc` — are
required; `cargo tauri dev` runs against host WebKitGTK):

```sh
cd crates/gui
npm --prefix ui install        # first time only
cargo tauri dev                # launch the GUI against source
cargo tauri build --bundles appimage   # produce the AppImage fallback
```

For the canonical Flatpak build and the CI packaging pipeline, see
[`docs/gui-packaging.md`](docs/gui-packaging.md).

### Development gates

Every change must pass:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## Domain model & decisions

- [`CONTEXT.md`](CONTEXT.md) — the project's ubiquitous language (glossary). Use these
  exact terms in code, issues, and tests.
- [`docs/adr/`](docs/adr/README.md) — architectural decision records, indexed by the area
  each one touches.
- [`docs/research/`](docs/research/) — dated research notes (package inventory, Plasma
  capture, Rust crate choices).
- [`docs/gui-packaging.md`](docs/gui-packaging.md) — how the `crates/gui` Tauri app ships on
  Linux (Flatpak canonical, AppImage fallback).

See [`AGENTS.md`](AGENTS.md) for the full contributor guide.
