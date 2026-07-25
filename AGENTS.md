# linux-backup-tool — Agent Guide

A Rust tool that backs up and restores a single Linux user's **environment** — program configs,
SSH keys, KDE Plasma layout, shell setup, user directories, package lists, and hand-picked paths
— into one portable archive, plus the manifest that makes restore possible. Two interfaces over
one domain library: a ratatui terminal app and a Tauri desktop app.

Read [CONTEXT.md](CONTEXT.md) for the domain glossary before touching anything. Architecture
decisions live in [docs/adr/](docs/adr/README.md); the judgment-level rules a linter can't check
are in [CODING_STANDARDS.md](CODING_STANDARDS.md); [README.md](README.md) is the user-facing
stack and build summary.

> **Maintaining this file:** if a fact has a home in code, link to it — don't copy it. Every
> restated fact is a future lie. Enumerations that mirror a directory, a config file, or a CI
> workflow (ADR lists, module lists, lint lists, skill lists) belong to their source, not here.
> This guide routes; it doesn't mirror. The one exception is [Known gaps](#known-gaps), which
> exists precisely for facts *not* derivable from the files.

## Where do I start?

| I want to…                                    | Start here                                                                    |
| --------------------------------------------- | ----------------------------------------------------------------------------- |
| Change what gets backed up / how it's captured | `crates/core/` — `catalog.rs` (allowlists), `scan.rs`, `packages.rs`          |
| Change archive / encryption / manifest format  | `crates/core/` — `archive.rs`, `crypto.rs`, `manifest.rs` + [Invariants](#invariants) |
| Change restore behavior                       | `crates/core/restore.rs` + [ADR 0002](docs/adr/0002-backup-restore-failure-asymmetry.md) |
| Change the terminal UI                        | `crates/tui/src/` — one `App` reducer, see [ADR 0005](docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md) |
| Change the desktop UI (Rust side)             | `crates/gui/src/` — commands + DTOs, see [ADR 0008](docs/adr/0008-gui-tauri-entry-point-core-boundary.md) |
| Change the desktop UI (React side)            | [crates/gui/ui/](crates/gui/ui/) — see [Frontend](#frontend)                  |
| Add or change a run fact (`home`, `now`, uid…) | `core::context::RunContext` — never read it ambiently, see [Invariants](#invariants) |
| Add/read an architecture decision             | [docs/adr/](docs/adr/README.md) — read before changing, see [Off-limits](#off-limits) |
| Package or ship the app                       | [docs/gui-packaging.md](docs/gui-packaging.md)                                 |
| Work an issue / PRD                           | [Agent skills](#agent-skills)                                                 |

## Setup

```sh
cargo build --workspace            # Rust 1.95+ (the floor `sysinfo` forces; CI pins 1.95.0)
npm --prefix crates/gui/ui install # frontend deps; Node major is pinned in crates/gui/ui/.nvmrc
```

The GUI needs a dev-time `webkit2gtk-4.1` and is driven through the Tauri CLI from
`crates/gui/`; `cargo run -p tui -- backup` runs the terminal wizard. Exact commands live in
[README.md](README.md#building) — read there rather than trusting a copy here. Frontend scripts
are defined in `crates/gui/ui/package.json`; the one that isn't self-evident is
`npm run build` = `tsc -b && vite build`, so it is the type-check gate as well as the bundler.

## Validation gate

Before considering any task done — all five, in any order:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo +1.95.0 check --workspace --all-targets --locked   # the MSRV floor
npm --prefix crates/gui/ui run lint && \
  npm --prefix crates/gui/ui run test && \
  npm --prefix crates/gui/ui run build                   # oxlint + vitest + tsc
```

**These five mirror the five jobs in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
one-to-one.** That is the contract: if you change one side, change the other in the same commit.
Two details worth knowing rather than rediscovering:

- **`--locked` is not decoration.** Without it a local run can silently rewrite `Cargo.lock` and
  pass where CI, which always passes it, fails.
- **The MSRV job is a real gate, and stable won't catch it.** It compiles against exactly
  1.95.0, without `-D warnings`, answering only "does it still build on the floor?". A
  post-1.95 API passes every other gate here and fails that one.

[`.github/workflows/packaging.yml`](.github/workflows/packaging.yml) (Flatpak + AppImage) is
separate and heavier, triggered only on GUI-affecting paths. It is not part of this gate; its
failure mode is "the shipped artifact is broken", not "the change is wrong".

## Project structure

`crates/core` (domain), `crates/tui` (terminal), `crates/gui` (desktop). Module names are mostly
self-describing — `core`'s public modules are listed in `crates/core/src/lib.rs` and each carries
a header comment that is its spec. Read those headers; they're more current than any summary.

The places where the layout misleads:

- **[crates/gui/ui/](crates/gui/ui/)** — a full React app living *inside* a Rust crate
  directory, with its own `package.json`, lockfile, lint, test runner, and Node version. It is
  not covered by any cargo command. See [Frontend](#frontend).
- **`crates/gui/gen/`** — generated Tauri ACL/capability schemas, regenerated by `build.rs` on
  every build. Compiler output, gitignored, like `target/`.
- **`crates/gui/ui/dist/index.html`** — a *tracked placeholder*, force-added into an otherwise
  ignored `dist/`, so `cargo build --workspace` succeeds on a clean checkout without a frontend
  build. Read its comment before touching it. See [Off-limits](#off-limits).
- **`crates/tui/src/system.rs`** and **`crates/gui/src/run_context.rs`** — near-identical by
  design, not by accident (ADR-0008). Don't DRY them.
- **`core::location`** — output-location resolution *and* writability refusal, not just path
  math. It is where a run decides it cannot proceed.

Also: [.agents/skills/](.agents/skills/) (agent skills, symlinked into `.claude/skills/`),
[docs/adr/](docs/adr/README.md), [docs/agents/](docs/agents/) (issue tracker, triage labels,
domain docs), [docs/research/](docs/research/) (dated notes — check for an existing one before
re-researching, but see [Known gaps](#known-gaps) first), [docs/prototypes/](docs/prototypes/).

## Invariants

The rules that span modules and will silently bite you:

- **`core` reads nothing from the ambient system** ([ADR 0004](docs/adr/0004-core-runcontext-injection.md)).
  Every run fact — `home`, output location, privilege, `host`, `user`, `uid`, `os`, `now` —
  arrives through an injected `RunContext`, filled in by `tui`'s and `gui`'s ambient probes or by
  tests with fixed values. Never call `std::env`, `dirs`, `hostname`, `getuid`, or the clock
  inside `core`; add a `RunContext` field instead. This is what makes the round-trip testable at
  all.
- **`core` is synchronous and owns all filesystem content I/O**
  ([ADR 0005](docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md),
  [0008](docs/adr/0008-gui-tauri-entry-point-core-boundary.md)). The UI crates own concurrency —
  `tui` a worker+channel+Tick pattern, `gui` `spawn_blocking` — and never read or write backup
  content themselves.
- **`core` has zero UI dependencies, compiler-enforced** ([ADR 0001](docs/adr/0001-core-tui-split-no-dedup-trait.md)).
  Its `Cargo.toml` must never gain a terminal, rendering, or Tauri crate.
- **Backup and restore fail differently, on purpose** ([ADR 0002](docs/adr/0002-backup-restore-failure-asymmetry.md)).
  Backup fails fast and cleans up after itself; restore is best-effort and reports per-file
  failures. Don't "fix" the asymmetry.
- **The Backup dir is the collision unit.** A run refuses if it already exists — it never merges
  into or overwrites one.
- **Encryption is all-or-nothing and never persisted.** Archive and manifest are both encrypted
  or both aren't; the Profile stores no encryption state, and Auto-insist is recomputed each run.
- **Everything crossing the IPC boundary is a DTO built by `From`**
  ([ADR 0008](docs/adr/0008-gui-tauri-entry-point-core-boundary.md)) — zero net-new `Serialize`
  derives on `core` types. Errors flatten through `SerializableCoreError`; its `kind` tags are a
  frontend contract (see [CODING_STANDARDS.md](CODING_STANDARDS.md#error-handling)).
- **Exclusion is prefix-wins, over one flat two-set model**
  ([ADR 0006](docs/adr/0006-recursive-drill-down-exclusion-model.md)). Recursive drill-down to
  any depth reuses it — there is no per-child carve-out of an excluded parent, and no Manifest or
  Profile change was needed for depth.
- **The wizard is a completion-frontier stepper** ([ADR 0007](docs/adr/0007-wizard-navigation-cross-step-invalidation.md)).
  A WHAT-step change invalidates the encryption decision in a targeted way; a set passphrase is a
  safe harbor. Cross-step invalidation is the part that breaks quietly.

## Testing

Follow the in-repo [TDD skill](.agents/skills/tdd/). Assert externally-observable behavior
through public APIs, not implementation details.

Where tests go, per crate — the counts and file names live in the tree, not here:

- **`core`** — one round-trip seam at `crates/core/tests/roundtrip.rs`, through `core`'s public
  API only. **Extend it rather than adding a second seam**; that single-seam rule is `core`'s,
  not the repo's. Unit tests live in `#[cfg(test)]` modules beside their source.
- **`gui`** — DTO/integration tests in `crates/gui/tests/`, exercising the same conversions the
  IPC commands return. Adding one here is normal and expected.
- **`gui/ui`** — vitest, co-located next to the hook or component under test
  (`useRun.test.tsx` beside `useRun.ts`). Test hook logic and non-trivial component behavior,
  not layout or style.
- **`tui`** — the reducer is pure by construction (ADR-0005), so test `update` against events
  rather than driving a terminal.

Test code is exempt from the no-`unwrap` lint via the crate-root and `tests/*.rs` allows already
in place — don't widen those beyond test code.

`RunContext` injection is what makes all of this possible: tests build a **Fixture environment**
(see [CONTEXT.md](CONTEXT.md)) in a tempdir with fixed run facts. Never reach for the real `$HOME`.

## Frontend

[crates/gui/ui/](crates/gui/ui/) is React 19 + TanStack Query + Zustand + Tailwind, with shadcn
components on Radix primitives. Dependencies and their versions live in `package.json`.

- **It reflects `core` state; it never owns domain state.** Selection, encryption decision, and
  run progress live behind IPC commands; the React side caches them via TanStack Query. Zustand
  holds view state only (route, wizard cursor, input buffers) — the same split ADR-0005 draws for
  `tui`.
- **Never hand-craft files in `src/components/ui/`** — the shadcn CLI owns them. Use the pinned
  CLI (`components.json` records the configured style), not `@latest`.
- **`dist/` is build output with one tracked exception.** If `npm run build` leaves
  `dist/index.html` modified, restore it (`git checkout -- crates/gui/ui/dist/index.html`) rather
  than committing a real build.
- The frontend has its own gate; see [Validation gate](#validation-gate). No cargo command
  reaches it.

## Commit conventions

[Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`, imperative
mood, lowercase summary, no trailing period.

```
feat(tui): add cross-screen keymap overlay
fix(gui): restore dist/index.html to the tracked placeholder
docs(adr): record explorer tri-state stays Category-agnostic
build(gui)!: rename the app id to io.github.payouri.linux-backup
```

- **Types**: `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `build`, `ci`, `chore`. `feat`
  and `fix` are for behavior changes only; everything else takes the closest match.
- **Scope** names the crate (`core`, `tui`, `gui`) or subsystem (`plasma`, `wizard`, `manifest`,
  `keymap`, `packaging`). Omit only when genuinely cross-cutting.
- **Breaking changes**: `!` after the type/scope, plus a `BREAKING CHANGE:` footer explaining it.
- **Issue links go in the footer**, never the summary: `refs #N`, or `closes #N` to auto-close.

## Git workflow

**`trunk` only moves by fast-forward, and code is pushed to it directly — no PR required unless
asked.** Issues and PRDs are tracked on GitHub; code doesn't route through PRs.

- Rebase your branch onto `trunk` (`git rebase trunk`), resolving conflicts **on the branch** —
  never merge `trunk` into it. Then `git merge --ff-only` and push.
- **Re-run the [validation gate](#validation-gate) on the rebased tip**, not just on the branch
  before rebasing. A clean textual rebase can still break a non-exhaustive match or a moved
  signature.
- CI runs on push to `trunk` as well as on PRs, so on the direct-push path it reports *after* the
  fact. The local gate is the real check.
- **Dependabot PRs are not merged.** Rebase the commit onto `trunk`, push, then comment on the PR
  naming the commit and close it — same record-keeping rule as issues below.
- **Before closing an issue, comment on it first** with the commit link and a short summary of
  what landed (`gh issue comment <n>`, then `gh issue close <n>`). The comment is the record;
  don't rely on the commit message once the issue is closed.
- **Delete the branch and remove the worktree when you're done** (`git worktree remove <path>`,
  `git branch -d <name>`) — after confirming the work reached `trunk`. Verify that with `git`,
  not with an issue comment claiming it.

## Off-limits

Do not modify without explicit instruction:

| Path                                | Reason                                                            |
| ----------------------------------- | ----------------------------------------------------------------- |
| `CONTEXT.md`                        | Domain glossary — only the project owner evolves it                |
| `Cargo.lock`, `ui/package-lock.json` | Never hand-edit — only `cargo`/`npm` write them                   |
| `crates/gui/ui/dist/index.html`     | Tracked placeholder, not build output — read its comment first     |
| `crates/gui/gen/`                   | Generated by `build.rs` on every build                            |
| `.github/workflows/*.yml` pins      | Actions are pinned by commit SHA on purpose; don't float them back |

**ADRs (`docs/adr/*.md`) are append-only, not frozen:**

- A **superseded** ADR stays on disk unmodified; the new ADR says it supersedes it, and the old
  one gets a `Superseded by 000N` marker. Never rewrite the old decision.
- An ADR that **amends** another (0009 amends 0005's worker pattern) is a new ADR that says so.
- Reversing a decision or changing rationale is always a **new ADR**.
- **Numbering**: highest existing number in `docs/adr/` + 1, zero-padded. Never reuse a number.
- Register the new ADR in [docs/adr/README.md](docs/adr/README.md), grouped by concern. That file
  is the index — this one deliberately doesn't keep a second copy.

## Known gaps

Facts not derivable from the files themselves. Everything else — read the directory.

- **[docs/research/rust-crates.md](docs/research/rust-crates.md) is partly superseded.** Its
  recommended set names `inquire` as the prompt crate and files `ratatui` under "deliberately not
  chosen"; the repo is ratatui + Tauri and `inquire` is gone (ADR-0005, ADR-0008 decided
  otherwise). Its tar → zstd → age pipeline notes and crate-vetting *pattern* still hold. The
  live record of dependency decisions is the comments in each `Cargo.toml`.
- **Issue #17 ("Alpha implementation spec") is closed** and predates the GUI. Its Testing
  Decisions section is no longer the source of truth — [Testing](#testing) above is.
- **`CONTEXT.md` predates the GUI.** Its glossary covers the domain (Entry, Selection, Manifest,
  …) but not the GUI/IPC layer. Use the ADR-0008/0009 vocabulary for those (DTO, IPC boundary,
  wizard step, `PendingProbe`, run outcome, collision reply) and flag the gap rather than coining
  a synonym — `CONTEXT.md` is owner-only.
- **`docs/agents/triage-labels.md` still carries template boilerplate** ("Label in
  mattpocock/skills", "Edit the right-hand column…"). The label strings in it are correct.
- **`docs/agents/issue-tracker.md`'s "PRs as a request surface" flag is `no`** — external PRs are
  not triaged as feature requests here.
- **One merge commit exists on `trunk`** (`9590b7b`, the #18 workspace scaffold), from before the
  fast-forward-only rule. It is not a precedent.

## Agent skills

Skills live in [.agents/skills/](.agents/skills/), symlinked into `.claude/skills/` and pinned by
`skills-lock.json`. Their names and descriptions are surfaced to you automatically — read the
directory rather than a list here.

- **`/rust-skills` is required for Rust work** — invoke it before writing, reviewing, or
  refactoring Rust, not optionally. [CODING_STANDARDS.md](CODING_STANDARDS.md) narrows its
  general rules to this workspace and wins where they overlap.
- `/tdd` and `/code-review` gate the code; `/triage`, `/to-tickets`, `/to-spec` and `/implement`
  operate against the GitHub tracker below.
- **Issue tracker** — issues and PRDs are GitHub issues in `payouri/linux-backup-tool`, via the
  `gh` CLI. See [docs/agents/issue-tracker.md](docs/agents/issue-tracker.md).
- **Triage labels** — five canonical triage roles plus `epic` for parent issues. See
  [docs/agents/triage-labels.md](docs/agents/triage-labels.md).
- **Domain docs** — single-context: one `CONTEXT.md` + `docs/adr/`. See
  [docs/agents/domain.md](docs/agents/domain.md).
