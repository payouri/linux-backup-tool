# Recursive drill-down reuses the flat two-set exclusion model; no Manifest change, no Profile migration, no carve-outs

**Status:** Accepted · **Date:** 2026-07-18

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

## Addendum: 2026-07-22 — `Vec<Entry>` → `BTreeMap<PathBuf, Entry>` (issue #82)

Decision 1 kept `entries`/`withheld` as flat collections and Decision 5 gave `core` exactly one
derived query, `directory_state`, computed by scanning them. Both held as written, but "scanning
them" turned out not to scale: `directory_state` was O(n) per call and got called once per visible
explorer row (O(rows × entries) per render), and `deselect_root` was O(n) per call while the WHAT
screen's Category-off toggle calls it once *per Entry* in the Category — O(paths × entries),
which froze the UI for a real ~40,000-file KDE Plasma icon theme. This is exactly the O(n²) shape
Decision 1's sibling `select_category` fix (the #30 `BTreeSet`-membership fix, predating this ADR)
already solved once for the *select* direction; #82 is the same shape on the *deselect* side, and
Decision 5's own escape clause — the derived query "lives in core... rather than each UI screen
re-deriving the withheld rule" — anticipates exactly this: a second consumer with a real
performance requirement licenses hardening the query's implementation, not just adding it.

**What changed.** `entries`/`withheld` are now `BTreeMap<PathBuf, Entry>` keyed by
`relative_path`, still the *sole* source of truth Decision 1 requires (no second structure, no
stored tri-state) — a `BTreeMap` is an ordered index over the same flat facts, not a different
model of them. Keying by `relative_path` promotes an invariant every prior linear-scan
implementation already relied on implicitly (each only ever found/acted on the *first* Entry
matching a given path): `relative_path` is unique across a Selection.

The prefix queries (`directory_state`, `deselect_root`, `reselect_root`,
`is_under_deselected_root`, and the `deselected_roots` cleanup inside `deselect_category`) are now
sorted-key range operations instead of `Path::starts_with` scans over every Entry:

- A Subtree's sorted-key range is `[prefix, prefix_upper)`, where `prefix_upper` appends a `0x00`
  byte (never a legal Unix path-component byte) to `prefix`'s last component — a bound that always
  sorts after every real descendant (however deep) and before the next sibling, decided at that one
  component regardless of remaining depth. `directory_state` and the `deselect_root`/
  `deselect_category` no-op guards use this as an O(log n) existence check (`Range::next`, stopping
  at the first match); `deselect_root`/`reselect_root` use it to collect exactly the Subtree's keys
  (`O(log n + k)`) and remove them individually (`O(k log n)` total) — deliberately *not*
  `BTreeMap::split_off`, whose single-split cost is O(log n) but whose remainder has to be merged
  back with `append`, which is linear in the combined size of both maps: for the common
  one-Entry-at-a-time call pattern, that merge alone would put the O(paths × entries) freeze right
  back.
- `is_under_deselected_root` walks `relative_path`'s ancestors (`Path::ancestors`, bounded by path
  depth) doing an O(log m) `BTreeSet` lookup at each level, rather than testing every one of the
  `m` deselected roots — `deselected_roots` has no path index of its own to range over, so this is
  the cheap side of that particular check.

**Consequence: `entries()` is now path-ordered, not scan-ordered**, and returns `Vec<&Entry>`
(there is no contiguous `Entry` storage left to hand back as a slice). This is benign by
Decision 3's own reasoning: the Manifest's `entries` array records *what* was selected, not an
order anything replays — restore extracts the tar stream directly, `checksums` is already a sorted
`BTreeMap`, and `verify` doesn't depend on sequence. The only observable effects are cosmetic: the
Manifest's `entries` array and tar-entry order shift from scan order to path order, and
`archive_sha256` (a hash of the whole `.tar.zst` stream) changes value accordingly — no schema
change, no new field, nothing a consumer reads positionally.

**No Profile change.** `deselected_paths`/`deselected_roots` (Decision 4) stay exactly the
persisted `Vec<PathBuf>` shape; the `BTreeMap` is in-memory only, built fresh from them (and from a
fresh scan) on load. No migration, no `SCHEMA_VERSION` bump — consistent with Decision 3's "the
Archive itself is the record" and Decision 4's "no new field and no migration" already covering any
representation the *in-memory* model chooses for the same persisted facts.

## Addendum: 2026-07-23 — explorer tri-state stays Category-agnostic (issue #86)

Issue #86 asked whether the recursive Drill-down explorer's tri-state checkbox, at a directory
node whose Subtree is shared by more than one Category (e.g. `.config`, `.local`), should keep
showing the Category-agnostic union (today's behavior, preserved by #83's AC) or narrow to only
the focused Category's Entries.

**Decision: stays Category-agnostic. No code change.**

`directory_state`/`deselect_root`/`reselect_root` (Decision 5) are and remain Category-agnostic: a
toggle at a shared node touches every Entry under that prefix regardless of Category. A
per-Category-scoped checkbox was considered and rejected because it would show a state the toggle
does not honor — decoupling display from effect, the same "second representation of the same
truth" Decision 1 already ruled out, this time keyed by Category rather than by directory. Making
the *toggle* itself Category-scoped to match would need a real model change (per-Category subtree
exclusion is not expressible in the flat two-set model at a shared prefix without either falling
back to per-Entry deselection at scale or introducing a Category-aware exclusion registry) — out
of proportion to a display question, and nothing in the ticket's motivation licensed it.

**Accepted tradeoff, recorded for future contributors:** at a shared node, a Partial checkbox can
come entirely from a Category other than the one being viewed. Toggling it still deselects the
whole Subtree across every Category sharing that prefix — including a Category whose own Entries
there were fully selected. This is a direct, correct consequence of Decision 5's Category-agnostic
`directory_state`/`deselect_root` pairing, not a bug: the checkbox tells the truth about what the
toggle does. A future UI affordance to communicate "this toggle also affects other Categories" is
a separate, later concern with no ticket currently open for it.
