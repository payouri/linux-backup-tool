# linux-backup-tool

A Rust desktop tool for backing up and restoring a Linux user's environment — program configs, SSH keys, KDE Plasma layouts, shell setups, user directories, installed-package manifests, and custom paths — with smart interactive defaults and a manifest that makes restore possible.

## Domain model

Read before working on any feature:

- **`CONTEXT.md`** at the repo root — the project's ubiquitous language (glossary). Use these exact terms in code, issues, and tests.
- **`docs/adr/`** — architectural decision records. Read the ones touching your area before changing it, and flag contradictions rather than silently overriding:
  - [`0001`](docs/adr/0001-core-tui-split-no-dedup-trait.md) — `core`/`tui` Cargo split; no `ArchiveBackend` trait until a second backend exists.
  - [`0002`](docs/adr/0002-backup-restore-failure-asymmetry.md) — backup fails fast and cleans up; restore is best-effort and reports per-file failures.
  - [`0003`](docs/adr/0003-plasma-raw-config-copy-not-konsave.md) — Plasma capture copies raw config via a curated allowlist; no `konsave` dependency.
  - [`0004`](docs/adr/0004-core-runcontext-injection.md) — `core` takes a full injected `RunContext`, reads nothing ambient (see Testing gates below).
  - [`0005`](docs/adr/0005-ratatui-app-architecture-core-tui-boundary.md) — the ratatui TUI is one `App` reducer in `tui`; core stays synchronous and owns all filesystem content I/O.
  - [`0006`](docs/adr/0006-recursive-drill-down-exclusion-model.md) — recursive drill-down reuses the flat two-set exclusion model; no Manifest change, no Profile migration.
  - [`0007`](docs/adr/0007-wizard-navigation-cross-step-invalidation.md) — the wizard is a completion-frontier stepper with targeted WHAT→encryption invalidation (set passphrase = safe harbor).
- **`docs/research/`** — dated research notes backing early tickets (package inventory, Plasma capture, Rust crate choices). Check for an existing note before re-researching a settled question.

## Gates

Checks a change must pass before it's considered done — run them, don't just assume:

- **`cargo fmt --check`** and **`cargo clippy --workspace --all-targets -- -D warnings`** — required, not advisory. Run both before considering a change done.
- **`cargo test --workspace --all-targets`** — must pass, including the round-trip seam below.

Read before writing or changing tests:

- **`core` reads nothing from the ambient system** — all run facts (`home`, output location, privilege, `host`, `user`, `uid`, `os`, `now`) arrive via an injected `RunContext`. Never call `std::env`, `dirs`, `hostname`, `getuid`, or the clock inside `core`; add a `RunContext` field instead. See `docs/adr/0004-core-runcontext-injection.md`.
- **One round-trip seam** through `core`'s public API (`crates/core/tests/roundtrip.rs`), asserting externally-observable behavior only — extend it rather than adding new seams. Full testing decisions live in issue #17 (Testing Decisions section).

## Git workflow

- **`trunk` only moves by fast-forward.** No merge commits onto `trunk`, ever.
- Before merging a branch, **rebase it onto `trunk`** (`git rebase trunk`), resolving any conflicts on the branch — not by merging `trunk` into it.
- Only merge/push once the rebased branch fast-forwards cleanly (`git merge --ff-only`).
- **Before closing an issue, comment on it first** with the commit link (`gh issue comment <n> --body "..."`) and a short summary of what changed, then close it. The comment is the record of what actually landed — don't rely on the commit message alone once the issue is closed.
- **Remove a git worktree once you're done with it** (`git worktree remove <path>`), rather than leaving it checked out. Confirm the branch has been merged/pushed first if the work should be kept.

## Coding conventions

- **Consult the `/rust-skills` skill** for idiomatic Rust guidance (ownership, error handling, async, API design, performance, testing, etc.) before writing, reviewing, or refactoring Rust code. The conventions below take precedence where they overlap or narrow its general rules.
- **Comments explain why, never what or how.** Well-named code already says what/how; a comment earns its place only by carrying a reason the code can't: a non-obvious constraint, an invariant, a workaround for a specific bug. If removing it wouldn't confuse a future reader, don't write it.
- **Error handling**: `Result`/`?` throughout, no `unwrap()`/`expect()`/`panic!()` outside tests and `main`. `core` errors use `thiserror`; `tui`'s boundary uses `anyhow`. Don't mix the two within a crate.
- **`core`'s public API stays minimal.** Default everything private; promote to `pub` only when `tui` (or a future `gui`) actually needs it.
- **No `unsafe` without justification.** `core` carries `#![forbid(unsafe_code)]`; if `unsafe` is ever unavoidable elsewhere, a comment must state why (see the why-not-what rule above).
- **New dependencies need a one-line justification** (health/maintenance, why this crate over alternatives) — follow the pattern in `docs/research/rust-crates.md` rather than adding a dep ad hoc.
- **No premature abstraction.** Don't introduce a trait/interface for a single implementation "for cleanliness" — see ADR-0001's `ArchiveBackend` reasoning, which generalizes: the abstraction waits for a second real implementation.
- **Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)**: `type(scope): summary` in imperative mood, lowercase summary, no trailing period.
  - **Types**: `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `build`, `ci`, `chore`. Use `feat`/`fix` only for behavior-changing changes; everything else takes the closest matching type.
  - **Scope** (optional but preferred) names the affected area — typically the crate (`core`, `tui`) or a subsystem (`plasma`, `wizard`, `manifest`, `keymap`). Omit it only when a change is genuinely cross-cutting.
  - **Breaking changes**: append `!` after the type/scope (`feat(core)!: …`) and explain the break in a `BREAKING CHANGE:` footer.
  - **Issue links** go in the footer, not the summary: `refs #N` for related work, `closes #N` to auto-close the originating issue.
  - Example: `feat(tui): add cross-screen keymap overlay` with a `refs #51` footer.

## Agent skills

### Rust coding practices

Use `/rust-skills` for Rust best practices (265 rules across ownership, error handling, async, API design, unsafe code, performance, testing, and more) — see the Coding conventions section above.

### Issue tracker

Issues live in GitHub Issues (repo `payouri/linux-backup-tool`) via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five canonical roles plus an `epic` label for parent issues. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
