# Recursive drill-down reuses the flat two-set exclusion model; no Manifest change, no Profile migration, no carve-outs

Drill-down is being extended from its alpha shape (descend from a Category into its Entries,
two levels) to **recursive** drill-down: descend into subdirectories to any depth and deselect
what you find. This ADR fixes how the domain model represents a deep deselection so it is honored
at backup and faithfully restored. It resolves
[#48](https://github.com/payouri/linux-backup-tool/issues/48).

The load-bearing discovery: the model **already** expresses arbitrary-depth exclusion. The scan
(`crates/core/src/scan.rs`) produces one `Entry` per regular file, each with a full
`relative_path` from `$HOME`; there is no coarse "directory Entry". Deselection already runs
through two registries on `SelectionModel`, and `deselect_root` already accepts any path with at
least one Entry beneath it, matching component-wise at any depth. "Recursive" was a limitation of
the alpha *UI*, not the model. So this effort adds almost nothing to core — it confirms the
semantics, adds one derived query, and sharpens the language.

## Decisions

### 1. Representation: the existing flat two-set model, unchanged

A Selection's exclusions stay two flat sets on `SelectionModel`:

- `deselected: BTreeSet<PathBuf>` — exact file paths (issue #21).
- `deselected_roots: BTreeSet<PathBuf>` — subtree prefixes, at any depth (issue #38).

An Entry is withheld when its exact path is in `deselected`, or a member of `deselected_roots` is
a path-prefix of it. A deep deselection is simply a longer `PathBuf` in one of these sets — no new
field, no new type. We do **not** introduce a stored tri-state directory tree: it would be a
second representation of the same truth that must be kept consistent with the sets, which the
no-premature-abstraction rule (ADR-0001) rejects absent a consumer the derived query below cannot
serve.

### 2. Exclusion is prefix-wins; no carve-outs

Exclusion is monotone and prefix-wins: deselecting a directory withholds every Entry beneath it,
and re-inclusion cancels an exclusion only at the exact node it was applied. You cannot deselect a
parent and then re-include one child of it. If you want most of a directory but not one subtree,
you deselect that subtree directly at its own depth.

The rejected alternative — gitignore-style negation (layered include/exclude rules with
precedence) — would demand exactly the ordered rule-engine and tri-state complexity Decision 1
avoids, and would make the Profile round-trip and the `entries`/`withheld` derivation far harder to
reason about. Carve-outs are a **later effort** if real demand appears; the alpha ships the simple
model.

### 3. The Manifest records no exclusions; `SCHEMA_VERSION` stays 2

The Manifest records only what *was* selected (its `entries` list is the active entries at backup
time) and nothing about what was excluded. This is sufficient for faithful restore because
**the Archive itself is the record**: an excluded file is never read, never enters the tar, and
never appears in `checksums`, so restore (which extracts the tar stream directly, not a manifest
inclusion list) reproduces exactly what was backed up by construction. Adding an exclusion field
would record data with no consumer — restore has nothing to skip, `verify` has nothing to check —
so `SCHEMA_VERSION` stays **2**. (If a future audit or "repeat this exact backup" feature needs the
exclusion set, that is an additive schema bump then, with a real consumer.)

### 4. The Profile shape is unchanged; no migration

`profile.toml` already persists deselections as two flat vectors, `deselected_paths` and
`deselected_roots` (both `Vec<PathBuf>`, both `#[serde(default)]`). A deep exclusion is just a
longer path string in the same vector, so deep drill-down needs **no new field and no migration**:
old profiles load unchanged, and deep exclusions written by a newer build read as ordinary
deselections even on an older build. This is why Decision 5's naming keeps the persisted field name
`deselected_roots` — renaming it would silently drop old profiles' exclusions and reintroduce the
migration this decision avoids.

### 5. Boundary: tui builds the tree, core owns one derived query

The drill-down UI needs a navigable directory tree, but the model is a flat `Vec<Entry>`. The
navigable/renderable tree is **presentation grouping of already-scanned data** (no fresh
filesystem reads), so `tui` derives it from the flat entries and core exposes no nested-node type —
consistent with ADR-0005 keeping view structure out of core.

Core gains exactly **one** read-only derived query — `directory_state(prefix) -> { All, Partial,
None }` — computed over `entries`/`withheld`. This is a Selection *semantic* (it must agree with
what `deselect_root`/`reselect_root` do), so it lives in core as the single source of truth and is
unit-testable there, rather than each UI screen re-deriving the withheld rule and risking drift.
Core's public API grows by this one query and nothing else.

## Consequences

- **Backup honoring is the existing withheld pattern**, unchanged: deselected entries are physically
  removed from `entries` before `archive::build` reads them (`lib.rs` hands `selection.entries()`
  straight to the archive/manifest builders). A prefix deselection prunes the whole subtree; a file
  deselection skips one file. `archive.rs`, `scan.rs`, and `restore.rs` need no changes.
- **Restore needs no exclusion awareness at all** — it extracts the Archive, which already contains
  exactly the included files.
- Core's implementation work reduces to: the `directory_state(prefix)` query plus tests proving
  `deselect_root` at intermediate depths behaves (the existing tests only cover allowlist-root and
  exact-file deselection). The tui drill-down tree and its navigation are downstream work under the
  TUI-overhaul tickets (#49 navigation, #50 per-screen design), not part of this decision.
- **The `deselected_roots` name now under-describes the concept** (a "root" can be any subtree at
  any depth). The name is retained in code and the persisted Profile for backward compatibility;
  `CONTEXT.md` documents the broadened meaning under **Subtree deselection**. A future rename would
  be a migration and is out of scope here.
- A contributor will be tempted to build a tri-state directory tree or gitignore-style negation "to
  do drill-down properly." Don't — Decisions 1 and 2 chose the flat, prefix-wins model on purpose;
  the richer models are later efforts gated on real demand.
