# Coding Standards

Judgment-level rules a linter can't check. Mechanical style is enforced by `rustfmt.toml`,
`clippy.toml`, and `[workspace.lints.clippy]` in the root `Cargo.toml` — read those for the
lint policy itself; each entry there carries its own rationale. Workflow, testing, gates, and
architectural invariants live in [AGENTS.md](AGENTS.md). This file holds only what's left:
decisions that need taste.

Rust best practices beyond this file: use the `/rust-skills` skill before writing, reviewing,
or refactoring Rust. Where it and this file overlap, this file wins — it narrows the general
rules to this workspace.

## Naming

- Use the canonical terms from [CONTEXT.md](CONTEXT.md) in names, types, tests, and issue
  titles. Match the glossary; don't coin synonyms (an `Entry`, not an "item"; a `Selection`,
  not "choices"; a `Backup dir`, not an "output folder").
- Introducing a new domain concept? Add it to `CONTEXT.md` first, then name the code after it.
  A concept that isn't in the glossary and doesn't belong there is usually a sign the code is
  inventing vocabulary the project doesn't need.

## Comments

Comments explain **why**, never what or how. Well-named code already says what/how; a comment
earns its place only by carrying a reason the code can't — a non-obvious constraint, an
invariant, a workaround for a specific bug, a rejected alternative. If deleting it wouldn't
confuse a future reader, don't write it.

This is the house style and it is followed densely: `Cargo.toml` dependency comments,
`clippy.toml` lint reasons, and module headers across `core` are all load-bearing prose. Match
that register rather than the sparser convention you may be used to.

## Error handling

`Result` and `?` throughout. `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`unreachable!`
are lint-blocked outside test code (`clippy.toml`), so the rule enforces itself — what needs
judgment is the shape of the error types:

| Crate      | Type                                             |
| ---------- | ------------------------------------------------ |
| `core`     | `thiserror` — `CoreError`, one variant per real failure mode |
| `tui`      | `anyhow` at the boundary (`main`, runtime, ambient probes)   |
| `gui`      | `anyhow` internally, same role as `tui`'s                    |
| IPC → `ui` | `SerializableCoreError` — see below                          |

- **Don't mix `thiserror` and `anyhow` within a crate.** `core` never reaches for `anyhow`;
  `tui`/`gui` never define a `thiserror` enum.
- **`SerializableCoreError` is the only error type that crosses IPC** (`crates/gui/src/error.rs`,
  ADR-0008). It flattens a `CoreError` to `{ kind, message }`. Its `kind` tags are a **contract
  with the React frontend** — the UI switches on them. Adding, renaming, or repurposing a tag is
  a frontend-visible change: update `crates/gui/ui/` in the same commit.
- Never derive `Serialize` on a `core` type to make an error crossable. ADR-0008 rejects that
  outright; extend the flattening instead.

## Escape hatches

`unsafe`, `#[allow(...)]`, and casts are escape hatches, not defaults.

- `core` carries `#![cfg_attr(not(test), forbid(unsafe_code))]` — production code in `core`
  cannot contain `unsafe` at all, and its test-only `libc::geteuid()` probes are the reason the
  attribute is `cfg_attr`'d rather than plain `forbid`.
- `tui`/`gui` may use `unsafe` for the three libc syscalls they genuinely need
  (`getuid`/`geteuid`/`gethostname` — `crates/tui/src/system.rs`,
  `crates/gui/src/run_context.rs`). Anything new needs a comment stating why no safe crate
  covers it.
- An `#[allow]` goes on the **narrowest possible item**, with a comment stating why the rule is
  waived rather than followed. The model is the one on `tauri::generate_context!()` in
  `crates/gui/src/lib.rs`: it names the exact reason (panics live in a dependency's macro
  output, not in expressible logic) and covers one expression.
- Crate-root `#![cfg_attr(test, allow(clippy::disallowed_methods, clippy::disallowed_macros))]`
  and the unconditional equivalent at the top of each `tests/*.rs` are the sanctioned test
  exemptions. Don't widen them beyond test code.

## API surface

- **`core`'s public API stays minimal.** Default everything private; promote to `pub` only when
  `tui` or `gui` actually needs it. A `pub` item with a single caller inside `core` is a mistake.
- Prefer **deep modules** — a small interface hiding substantial behavior — over many shallow
  pass-throughs. `core::model::SelectionModel` is the shape to imitate: plain accessors out, all
  the exclusion logic behind them.

## Abstraction

- Run the **deletion test** before adding any abstraction: would removing it concentrate
  complexity in one place, or just move it around? If it only moves complexity, don't add it.
- **No trait for a single implementation.** ADR-0001 declined an `ArchiveBackend` trait on
  exactly this ground and the reasoning generalizes: the abstraction waits for the second real
  implementation.
- **`gui` and `tui` deliberately duplicate rather than share** (ADR-0008). `system.rs` and
  `run_context.rs` are near-identical on purpose. Don't DRY them into a shared crate — the
  extraction waits for a third consumer or a genuine third copy.

## Dependencies

Every dependency carries a `#` comment directly above it in `Cargo.toml` giving:

1. why this crate over the alternatives, and
2. its health signal (maintenance, or the constraint that forced the exact version).

Follow the existing entries — `secrecy = "0.10"` in `crates/core/Cargo.toml` (which explains why
*not* the version the research note recommends) is the standard to match. This convention, not
`docs/research/rust-crates.md`, is the live record of dependency decisions; that note is a
point-in-time survey and parts of it are superseded.
