# linux-backup-tool

An interactive Rust tool for backing up and restoring a Linux user's environment.

Backs up program configs, SSH keys, KDE Plasma layouts, shell configs and plugins,
user directories, installed-package manifests, and user-supplied custom paths — with
smart defaults you confirm interactively, a chosen output location on any writable
drive, and a manifest that records what was backed up so it can later be restored.

> 🚧 Early development. Design and planning are tracked as GitHub issues via
> [`/wayfinder`](docs/agents/issue-tracker.md).

## How it works

`linux-backup` is a forward-only terminal wizard: it scans your machine, proposes a
smart-default selection you refine, insists on encryption when the selection contains
anything sensitive, lets you pick where the backup lands, and writes a single
self-describing backup directory. Restore reads that directory back — verifying its
integrity, re-rooting home-tagged files under the restoring user, and preserving
ownership and permissions.

```
WHAT ──▶ [encryption] ──▶ WHERE ──▶ REVIEW ──▶ RUN
scan     auto-insist       pick       confirm    tar│zstd│age
```

Each backup produces one directory named `linux-backup-<host>-<timestamp>/` containing
the compressed (optionally encrypted) archive and a `manifest.json` describing exactly
what was captured.

## Commands

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

## Architecture

A two-crate Cargo workspace with a compiler-enforced split
([ADR-0001](docs/adr/0001-core-tui-split-no-dedup-trait.md)): all domain logic lives in
`core`, which carries **zero UI dependencies**; `tui` is a thin view on top.

```
crates/
├── core/   linux-backup-core — UI-free domain library (#![forbid(unsafe_code)])
└── tui/    tui — inquire-based terminal wizard → binary `linux-backup`
```

`core` reads **nothing** from the ambient system: every run fact (`home`, output
location, privilege, `host`, `user`, `uid`, `os`, `now`) arrives through an injected
`RunContext` ([ADR-0004](docs/adr/0004-core-runcontext-injection.md)). `tui`'s `main`
fills that context from the real machine; tests fill it with fixed values, which makes
the round-trip seam fully deterministic.

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
| `progress` | A callback trait `tui` implements to drive per-phase progress bars; `core` stays synchronous. |
| `error` | The typed `CoreError` surface (`thiserror`; `anyhow` is reserved for `tui`). |

Top-level `core::backup()` and `core::restore::restore()` are the public seams the
round-trip test drives. Backup **fails fast** — any error after the backup directory is
created deletes it, so a directory with a manifest in it is always complete. Restore is
**best-effort** — it continues past per-file failures and reports each one
([ADR-0002](docs/adr/0002-backup-restore-failure-asymmetry.md)).

### `tui` — the terminal wizard

An [`inquire`](https://crates.io/crates/inquire)-based wizard that owns only screen
loops, rendering, and cursor state — every decision is delegated to `core`.

| Module | Responsibility |
| --- | --- |
| `main` | Binary entry point: wires argv into a `Command` and dispatches. |
| `cli` | Hand-rolled argv parsing for the small subcommand grammar (no `clap`). |
| `select_cmd` | The WHAT screen: category checklist with 🔒/size annotations, drill-down into entries, and a live footer of selection size + auto-insist state. |
| `encryption_cmd` | The encryption step: sets a passphrase, or disables auto-insist behind a second plaintext-naming confirmation. |
| `where_screen` | The WHERE screen: output-location pick-list wired to real drive discovery and mounting. |
| `backup_cmd` | Chains the three screens into the end-to-end backup flow, owning REVIEW rendering and the `indicatif` progress-bar adapter. |
| `restore_cmd` | Orchestrates `core::restore`, prompting for the passphrase *before* any file is touched. |
| `system` | Builds a real `RunContext` from the actual machine (`$HOME`, hostname, uid/euid privilege, OS, wall clock). |

## Building

```sh
cargo build --release          # binary at target/release/linux-backup
cargo run -p tui -- backup     # run the wizard from source
```

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
- [`docs/adr/`](docs/adr/) — architectural decision records.
- [`docs/research/`](docs/research/) — dated research notes (package inventory, Plasma
  capture, Rust crate choices).

See [`AGENTS.md`](AGENTS.md) for the full contributor guide.
