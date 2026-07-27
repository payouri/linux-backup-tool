//! The `SelectionModel`: Category defaults, precedence resolution, Drill-down toggles, and
//! derived state (`encryption_required`, `total_size`, `selected_paths`, `warnings`).
//!
//! This is `core`'s authoritative model of what will be backed up; `crates/tui` is a thin
//! view over it, owning only cursor/scroll state and rendering.
//!
//! #20 scope: `SelectionModel::from_smart_defaults` composes a Smart-default Selection
//! across the full ten-Category taxonomy (`scan::scan_taxonomy`'s precedence + Hide-empty
//! result), and `warnings()` derives the User-data size warning (story 4) over whatever
//! Categories ended up in the Selection.
//!
//! #21 scope (CONTEXT.md's "Drill-down"): `deselect_entry`/`reselect_entry` let a specific
//! Entry within an otherwise-checked Category be excluded from the active Selection without
//! touching the Category-level checkbox (`from_smart_defaults`'s job). A deselected Entry is
//! physically moved out of `entries` into `withheld`, so every "what's actually selected"
//! accessor below (`selected_paths`, `total_size`, `warnings`, `encryption_required`, and the
//! `entries()` slice `backup()` hands to `archive::build`/the Manifest builder) sees it
//! disappear for free, with no per-accessor filtering to keep in sync. `withheld` keeps the
//! full `Entry` around so a Drill-down UI can re-list and reselect it; `deselected` is the
//! plain `BTreeSet<PathBuf>` of relative paths — the storable shape #34 (Profile persistence)
//! needs to remember deselections across runs (CONTEXT.md's "Profile").
//!
//! #38 scope (story 13, the same "Drill-down"): `deselect_root`/`reselect_root` deselect a
//! whole allowlist root (a directory Entry like KDE Plasma's `icons/`/`wallpapers/`) as one
//! toggleable unit, so dropping a ~40,000-file icon theme is a single action rather than
//! unchecking every expanded file. A root deselection is prefix-based — every Entry whose
//! `relative_path` is under the root moves to `withheld` — and is remembered in its own
//! `deselected_roots` set (the storable shape the Profile persists, one root path instead of
//! the tens of thousands it expands to). An Entry is withheld from the active Selection when
//! *either* registry excludes it: its exact path is in `deselected`, or one of
//! `deselected_roots` is a path-prefix of it (`is_under_deselected_root`). Every path that
//! (re)activates an Entry — `reselect_entry`, `reselect_root`, `select_category` — honors both
//! registries, so an Entry under a still-deselected root can never slip back into the active
//! Selection.
//!
//! #22 scope: `encryption_required` delegates to `crypto::evaluate`'s Auto-insist policy —
//! see that module for which specific Categories/Entries triggered it.
//!
//! #82 scope (ADR-0006's dated addendum): `entries`/`withheld` are `BTreeMap<PathBuf, Entry>`
//! keyed by `relative_path`, not `Vec<Entry>` — the sole source of truth for what's
//! active/withheld, so a Subtree query (`directory_state`, `deselect_root`, `reselect_root`,
//! `is_under_deselected_root`, and the `deselected_roots` cleanup inside `deselect_category`)
//! is a sorted-key range operation instead of a scan of every Entry. This is what makes
//! toggling a Category with tens of thousands of Entries instant rather than the O(n²) freeze
//! `select_category`'s doc below already fixed once for the *select* direction; #82 was the
//! same shape on the *deselect* side; see `prefix_range_end`'s doc for the range-bound trick.
//!
//! #83 scope: `active_entries_under`/`withheld_entries_under` expose #82's range machinery
//! directly to `crates/tui`, so the recursive Drill-down explorer's `explorer_rows` can range-
//! scan just the current breadcrumb's Subtree (`O(log n + k)` for the `k` Entries under it)
//! instead of filtering every Entry in the Selection per render — the last O(n)-per-render cost
//! #82 didn't already reach, because it lived in `tui`, not here.

use std::cell::Cell;
use std::collections::btree_map::Range;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::ops::Bound;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::catalog;
use crate::crypto;
use crate::scan::{self, CategoryScan};

/// The size threshold (story 4: ">1GB") above which a User-data Selection earns a warning.
const USER_DATA_WARNING_THRESHOLD_BYTES: u64 = 1_000_000_000;

/// What kind of filesystem object an Entry points at — recorded per-Entry in the Manifest
/// (story 43) and, for a [`EntryType::Symlink`], the signal that `archive::build` must
/// capture the link itself (its target preserved) rather than dereferencing it to the
/// target's bytes (issue #36).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryType {
    /// A regular file: its bytes are read and stored, and a per-file checksum is recorded.
    #[default]
    File,
    /// A directory.
    Directory,
    /// A symbolic link: archived as a symlink whose target path is preserved verbatim,
    /// never followed to the target's contents.
    Symlink,
}

impl EntryType {
    /// Classifies `path` by an lstat (`fs::symlink_metadata`), so a symlink is reported as a
    /// [`EntryType::Symlink`] rather than resolved to whatever it points at. An unreadable
    /// path (e.g. one that Vanished between add and classify) falls back to
    /// [`EntryType::File`]: `archive::build`'s per-Entry read is the single authoritative
    /// place a genuine read failure or a benign Vanish is decided (ADR-0002), never this
    /// classifier.
    pub(crate) fn classify(path: &Path) -> Self {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => Self::Symlink,
            Ok(meta) if meta.is_dir() => Self::Directory,
            _ => Self::File,
        }
    }
}

/// Whether an Entry's path is anchored under `$HOME` or stands on its own. See
/// CONTEXT.md's "Re-rooting" entry — this is the Manifest's `root` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRoot {
    /// Resolved relative to `RunContext.home` at backup time; re-based under whichever
    /// `$HOME` is active at restore time.
    Home,
    /// Restored verbatim, with no re-rooting.
    Absolute,
}

/// One selected root (a "selected root" per CONTEXT.md's "Entry" entry): a concrete,
/// already-resolved path plus the Manifest fields it will carry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The absolute, on-disk path this Entry resolved to at scan time.
    pub absolute_path: PathBuf,
    /// The path relative to its root (home or filesystem root `/`), used both as the
    /// Manifest's `entries[].path` and as the Archive's internal tar path.
    pub relative_path: PathBuf,
    /// Home-tagged or absolute — see [`EntryRoot`].
    pub root: EntryRoot,
    /// File, directory, or symlink — see [`EntryType`]. Set once when the Entry is
    /// constructed (at scan time, or by `profile::custom_entry` for a Custom path); carried
    /// verbatim into the Manifest's per-Entry `type` and consulted by `archive::build` to
    /// preserve a symlink rather than dereference it.
    pub entry_type: EntryType,
    /// The Category name this Entry belongs to.
    pub category: &'static str,
    /// Whether this Entry is Sensitive: true if its Category is Sensitive, OR it carries a
    /// per-entry sensitivity tag (`catalog::entry_is_individually_sensitive`), OR the
    /// Safety-net scan flagged its filename (`scan::is_safety_net_match`) — a strict
    /// superset of all three origins, per CONTEXT.md's "Sensitive" entry. Set once at scan
    /// time; never re-derived here.
    pub sensitive: bool,
}

impl Entry {
    /// The Entry's path *within the Archive*: `home/<relative_path>` or
    /// `absolute/<relative_path>`. This root-tag prefix is what lets `restore` Re-root a
    /// `home`-tagged Entry under a different `$HOME` purely by reading the tar stream,
    /// without consulting the Manifest's `root` field again.
    pub fn archive_path(&self) -> PathBuf {
        let root_prefix = match self.root {
            EntryRoot::Home => "home",
            EntryRoot::Absolute => "absolute",
        };
        Path::new(root_prefix).join(&self.relative_path)
    }
}

/// The tri-state of a directory node in the recursive Drill-down explorer (#48 / ADR-0006),
/// derived live from the Selection rather than stored — ADR-0006 keeps the flat two-set model
/// with no stored tri-state tree, so this is a computed view, never a field.
///
/// - `All` — every known Entry under the node is in the active Selection.
/// - `None` — every known Entry under it is withheld (a Subtree deselection, or per-file
///   deselections covering all of them), or the node covers no known Entry at all.
/// - `Partial` — some in, some out (CONTEXT.md's "Partial selection").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryState {
    All,
    Partial,
    None,
}

/// The user's chosen set of Entries for a run (CONTEXT.md's "Selection"). `core`'s
/// authoritative model of what will be backed up.
#[derive(Debug, Clone, Default)]
pub struct SelectionModel {
    /// The currently active Entries, keyed by `relative_path` — see the module doc's #21
    /// section for why every derived accessor reads only this field, and the #82 section for
    /// why a `BTreeMap` (a sorted-key index for free) rather than a `Vec`. Keying by
    /// `relative_path` assumes it's unique across the Selection; every prior linear-scan
    /// implementation already only ever found/acted on the first Entry matching a given
    /// `relative_path`, so this is a documented existing invariant, not a new one.
    entries: BTreeMap<PathBuf, Entry>,
    /// Entries deselected via Drill-down (per-file or by root), held here rather than dropped
    /// so `entries_in_category`/`reselect_entry`/`reselect_root` can still find them.
    withheld: BTreeMap<PathBuf, Entry>,
    /// The relative paths of Entries deselected individually (per-file Drill-down) — the
    /// storable shape exposed by `deselected_paths` for #34 to persist. A subset of the
    /// withheld Entries' paths: a root-level deselection withholds Entries without listing
    /// each here (that's `deselected_roots`' job).
    deselected: BTreeSet<PathBuf>,
    /// The allowlist roots deselected as whole units (#38) — a directory Entry like
    /// `.local/share/icons` recorded once instead of every file under it. Any withheld Entry
    /// whose `relative_path` starts with one of these is excluded by that root, not a per-file
    /// mark. The storable shape exposed by `deselected_roots` for the Profile to persist.
    deselected_roots: BTreeSet<PathBuf>,
    /// Lazily-populated `total_size()` result: `None` means "unknown, recompute on next call".
    /// Every method that changes which Entries are active (`add_custom_path`, `deselect_entry`,
    /// `reselect_entry`, `select_category`, `deselect_category`, `deselect_root`,
    /// `reselect_root`) calls `.set(None)` to invalidate it, so a stale total from a `stat` done
    /// before the Selection changed can never be handed back. Nothing invalidates it for an
    /// out-of-band disk mutation (a file growing between scan and backup) — `total_size`'s doc
    /// already scoped that as not its concern, and this cache doesn't change that contract, only
    /// how often the unchanged answer is re-`stat`ed. A `Cell` rather than `RefCell` because the
    /// cached value (`Option<u64>`) is `Copy`, so there's nothing to borrow-check; interior
    /// mutability is needed at all only because `total_size(&self)`'s signature can't change
    /// (WHERE/REVIEW render paths and `RunPlan::new` call it every render with a shared
    /// reference).
    total_size_cache: Cell<Option<u64>>,
}

impl SelectionModel {
    /// Builds a Selection from an already-scanned, already-selected set of Entries. Useful
    /// on its own for a single Category (as the round-trip seam's `shell_selection` helper
    /// does) or once Drill-down (#21) has finished toggling individual Entries within an
    /// otherwise Smart-defaulted Selection.
    pub fn new(entries: Vec<Entry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.relative_path.clone(), entry))
                .collect(),
            withheld: BTreeMap::new(),
            deselected: BTreeSet::new(),
            deselected_roots: BTreeSet::new(),
            total_size_cache: Cell::new(None),
        }
    }

    /// Builds a Selection from a full taxonomy scan (`scan::scan_taxonomy`), keeping only
    /// the Entries of Categories whose Smart default is ON (story 2) — the state the
    /// Selection surface opens in, before any user toggling or Drill-down deselection.
    ///
    /// `scanned` has already had precedence resolution and Hide-empty applied by
    /// `scan_taxonomy`, so this only has to filter on `smart_default`; it never re-decides
    /// which Category owns a path.
    pub fn from_smart_defaults(scanned: &[CategoryScan]) -> Self {
        let entries = scanned
            .iter()
            .filter(|scan| scan.category.smart_default)
            .flat_map(|scan| scan.entries.iter().cloned())
            .collect();
        Self::new(entries)
    }

    /// Adds a Custom path (CONTEXT.md's "Custom path", stories 6/8) to the active Selection.
    /// Builds its [`Entry`] with the same re-rooting and Safety-net sensitivity derivation a
    /// remembered Custom path gets when a Profile is restored ([`custom_entry`]) — so an
    /// interactively-added path and a persisted one are indistinguishable once in the model,
    /// and both feed `total_size`/`encryption_required`/the Manifest the same way.
    ///
    /// `absolute_path` is expected already expanded and already validated against the
    /// root/non-root rule (`catalog::validate_custom_path`) by the caller: this owns only the
    /// Entry construction and insertion, not the privilege policy (which needs the injected
    /// `RunContext.privilege` the caller holds). Re-adding a path already actively selected is
    /// a no-op rather than a duplicate, so the running total can't double-count one Custom
    /// path. Re-adding a path that is currently `withheld` (Drill-down toggled off, #106)
    /// **reactivates** it instead of no-op'ing or duplicating: typing a path and pressing
    /// Enter is an explicit "I want this backed up", so it routes through the model's own
    /// reversal paths (`reselect_root` for a directory Custom path, `reselect_entry`
    /// otherwise) rather than a raw re-insert, so nothing is left stranded contradicting
    /// `is_under_deselected_root`. Re-adding a path withheld only by a deselected *ancestor*
    /// root still won't reactivate — prefix-wins with no carve-outs (ADR-0006 Decision 2).
    ///
    /// The guard is keyed on `relative_path`, matching every sibling mutator, rather than the
    /// `absolute_path` scan this used to be: a colliding key (e.g. an Absolute-rooted `/foo`
    /// and a Home-rooted `~/foo`) now no-ops instead of silently overwriting the Category
    /// Entry already there. The Entry is built (one `lstat` via `EntryType::classify`) before
    /// the lookup, so the no-op path costs that probe it used to skip — negligible at one add
    /// per keypress.
    pub fn add_custom_path(&mut self, absolute_path: &Path, home: &Path) {
        let entry = custom_entry(absolute_path, home);
        let key = entry.relative_path.clone();
        if self.entries.contains_key(&key) {
            return;
        }
        if self.withheld.contains_key(&key) {
            match entry.entry_type {
                EntryType::Directory => self.reselect_root(&key),
                _ => self.reselect_entry(&key),
            }
            return;
        }
        self.entries.insert(key, entry);
        self.total_size_cache.set(None);
    }

    /// Removes a Custom path from the Selection entirely (CONTEXT.md's "Custom path", #106) —
    /// the "forget it" verb, distinct from a Drill-down withhold's "not this run". Keyed on
    /// `relative_path`, not `absolute_path`: every sibling mutator (`deselect_entry`,
    /// `deselect_root`, `reselect_root`) already keys that way, it is a direct `O(log n)`
    /// `BTreeMap` lookup rather than a scan, and it is the only path an Explorer row carries —
    /// re-rooting from a relative path would be wrong for an Absolute-rooted Custom path.
    ///
    /// No-op unless the Entry at that key belongs to [`catalog::CUSTOM_PATHS`]: keys are
    /// unique per map but not unique to the Custom-paths Category (a Programs Entry, or an
    /// Absolute-rooted path colliding with a Home-rooted one, can occupy the slot), so an
    /// unguarded removal could silently delete a Category Entry recoverable only by
    /// re-scanning. Checks `entries` before `withheld` — `custom_entry` never expands a
    /// directory into per-file Entries, so a directory Custom path is one Entry with no
    /// children of its own, and it sits in exactly one of the two maps.
    ///
    /// `deselected` residue is cleared unconditionally: the Entry is gone and no other Entry
    /// can hold the same key in the same map, so nothing can be stranded. `deselected_roots`
    /// residue is pruned only when nothing active or withheld remains under the key —
    /// reusing the retain-if-still-covers pass `deselect_category` already performs, via the
    /// same [`prefix_range_nonempty`] helper — so removing a directory Custom path that still
    /// shares its prefix with other Categories' Entries leaves those Entries exactly as the
    /// user last saw them (withheld, with the root deselection that excludes them intact)
    /// rather than silently re-selecting them.
    ///
    /// No validation: the add-time root/non-root privilege rule
    /// (`catalog::validate_custom_path`) is an add-time policy needing the injected
    /// `RunContext.privilege` the caller holds. Removal only deletes what the Category guard
    /// already proved is present, so it is unconditional.
    pub fn remove_custom_path(&mut self, relative_path: &Path) {
        let holds_custom = |map: &BTreeMap<PathBuf, Entry>| {
            map.get(relative_path)
                .is_some_and(|entry| entry.category == catalog::CUSTOM_PATHS.name)
        };
        let removed = if holds_custom(&self.entries) {
            self.entries.remove(relative_path)
        } else if holds_custom(&self.withheld) {
            self.withheld.remove(relative_path)
        } else {
            None
        };
        if removed.is_none() {
            return;
        }
        self.total_size_cache.set(None);
        self.deselected.remove(relative_path);
        if !prefix_range_nonempty(&self.entries, relative_path)
            && !prefix_range_nonempty(&self.withheld, relative_path)
        {
            self.deselected_roots.remove(relative_path);
        }
    }

    /// The active (non-deselected) Entries, in path order (the `BTreeMap` key order — #82's
    /// dated ADR-0006 addendum) — what `archive::build` and the Manifest builder actually
    /// consume. Order was scan order before #82 and is now path order; nothing downstream
    /// depends on the sequence itself (`checksums` is a sorted `BTreeMap`, and restore/verify
    /// extract from the tar stream rather than replaying this order), so only
    /// `archive_sha256` and the Manifest's `entries` array's cosmetic ordering shift.
    pub fn entries(&self) -> Vec<&Entry> {
        self.entries.values().collect()
    }

    /// Deselects (Drill-down, #21) the Entry whose relative path is `relative_path`,
    /// independently of its Category's own checkbox: the Entry moves out of the active
    /// Selection (so `selected_paths`/`total_size`/`entries()` stop seeing it) into
    /// `withheld`, where `entries_in_category`/`reselect_entry` can still find it. A no-op
    /// if no active Entry matches `relative_path` — already deselected, or never part of
    /// the Selection to begin with.
    pub fn deselect_entry(&mut self, relative_path: &Path) {
        let Some(entry) = self.entries.remove(relative_path) else {
            return;
        };
        self.deselected.insert(relative_path.to_path_buf());
        self.withheld.insert(relative_path.to_path_buf(), entry);
        self.total_size_cache.set(None);
    }

    /// Reverses `deselect_entry`: clears `relative_path`'s per-file deselection and moves the
    /// Entry back into the active Selection. A no-op if `relative_path` isn't currently
    /// withheld. If the Entry is *also* under a still-deselected root (#38), the per-file mark
    /// is cleared but the Entry stays withheld — the root deselection still excludes it, so it
    /// must not slip back into the active Selection.
    pub fn reselect_entry(&mut self, relative_path: &Path) {
        self.deselected.remove(relative_path);
        if self.is_under_deselected_root(relative_path) {
            return;
        }
        if let Some(entry) = self.withheld.remove(relative_path) {
            self.entries.insert(relative_path.to_path_buf(), entry);
            self.total_size_cache.set(None);
        }
    }

    /// Whether `relative_path` carries an individual (per-file) Drill-down deselection. This
    /// is the per-file registry only — an Entry withheld because a whole root above it was
    /// deselected (#38) is *not* reported here; use `is_root_deselected` for that.
    pub fn is_deselected(&self, relative_path: &Path) -> bool {
        self.deselected.contains(relative_path)
    }

    /// Whether `relative_path` falls under any currently-deselected allowlist root (#38) — a
    /// root path-prefix of it, `relative_path` itself included. Path-component-wise, so a
    /// deselected `.local/share/icons` covers `.local/share/icons/hicolor/x.svg` but not a
    /// sibling `.local/share/icons-backup`.
    ///
    /// Walked ancestor-by-ancestor (`Path::ancestors`, bounded by `relative_path`'s depth) with
    /// an `O(log m)` `BTreeSet` lookup at each level (#82), rather than testing `starts_with`
    /// against every one of the `m` deselected roots: `deselected_roots` has no path index of
    /// its own to range over (it's not keyed by directory nesting), but any root that covers
    /// `relative_path` must be one of its literal ancestors, so walking those is exactly the
    /// candidate set — no shorter path through the sorted order (an ancestor is always `<=`
    /// `relative_path` in the component order, but so are unrelated siblings, so a single
    /// `range`/`next_back` lookup can't distinguish an ancestor from a smaller sibling root).
    fn is_under_deselected_root(&self, relative_path: &Path) -> bool {
        relative_path
            .ancestors()
            .any(|ancestor| self.deselected_roots.contains(ancestor))
    }

    /// Category-level toggle ON (the WHAT screen's Category checkbox, #30): adds every
    /// Entry from `scan` into the Selection. An Entry already active is left untouched; one
    /// already `withheld` (a previous Drill-down deselection) is moved back to active via
    /// `reselect_entry` rather than duplicated — this is also what lets a Drill-down UI call
    /// this unconditionally before listing a Category's Entries, so `reselect_entry`/
    /// `deselect_entry` always have something to act on even for a Category whose Smart
    /// default was OFF and so has never been part of this model at all. Only a brand-new
    /// Entry (never seen by this model) is pushed straight into the active Selection.
    ///
    /// Membership is checked via a direct `BTreeMap::contains_key` per Entry, not a
    /// `Vec::iter().any(...)` per Entry: the naive per-Entry linear scan is O(current
    /// Selection size) each time, which turns loading a large Category into this model into
    /// an O(n²) scan — measured at ~88s for KDE Plasma's ~40,000-file icon theme (a real
    /// Category size, story 13) during #30's manual verification against a real account. #82
    /// replaced the `Vec`s (and the `BTreeSet` snapshot this method used to build from them
    /// up front) with `BTreeMap`s that already carry a path index, so each check below is an
    /// O(log n) lookup straight against `entries`/`withheld` with nothing to pre-snapshot.
    pub fn select_category(&mut self, scan: &CategoryScan) {
        let mut to_reselect = Vec::new();
        let mut to_insert_active = Vec::new();
        let mut to_insert_withheld = Vec::new();
        for entry in &scan.entries {
            let path = entry.relative_path.as_path();
            if self.entries.contains_key(path) {
                continue;
            }
            // A root-level Drill-down deselection (#38) survives a Category (re)select: its
            // subtree stays withheld even as the Category is turned on as a whole. Only a
            // per-file withhold is lifted here — so an Entry already withheld under a
            // still-deselected root is left where it is, and a brand-new Entry that lands under
            // one goes straight to `withheld` rather than the active Selection.
            if self.withheld.contains_key(path) {
                if !self.is_under_deselected_root(path) {
                    to_reselect.push(entry.relative_path.clone());
                }
            } else if self.is_under_deselected_root(path) {
                to_insert_withheld.push(entry.clone());
            } else {
                to_insert_active.push(entry.clone());
            }
        }

        for path in to_reselect {
            self.reselect_entry(&path);
        }
        if !to_insert_active.is_empty() {
            self.total_size_cache.set(None);
        }
        for entry in to_insert_active {
            self.entries.insert(entry.relative_path.clone(), entry);
        }
        for entry in to_insert_withheld {
            self.withheld.insert(entry.relative_path.clone(), entry);
        }
    }

    /// Category-level toggle OFF: removes every Entry belonging to `category_name` from the
    /// Selection entirely — both active and `withheld` — rather than only clearing active
    /// ones. Toggling back on (`select_category`) then re-adds the Category's full scanned
    /// Entry set fresh: the simplest checkbox semantics to reason about (an unchecked box
    /// carries no partial memory), even though it means a Drill-down deselection doesn't
    /// survive a full off/on cycle within the same run — an accepted simplification for the
    /// alpha's first Selection screen (#30); re-drilling after re-checking is cheap.
    pub fn deselect_category(&mut self, category_name: &str) {
        self.total_size_cache.set(None);
        self.entries.retain(|_, e| e.category != category_name);

        let removed_withheld: Vec<PathBuf> = self
            .withheld
            .iter()
            .filter(|(_, e)| e.category == category_name)
            .map(|(path, _)| path.clone())
            .collect();
        self.withheld.retain(|_, e| e.category != category_name);
        for path in removed_withheld {
            self.deselected.remove(&path);
        }
        // Drop any root-level deselection (#38) that no longer covers a remaining Entry: a
        // Category toggled fully off carries no partial memory — per-file or per-root — into
        // its next toggle-on, matching this method's existing "an unchecked box carries no
        // partial memory" contract above. #82: an O(log n) range-existence check against
        // `entries`/`withheld` per remaining root, rather than a scan of every surviving Entry
        // per root — that nested shape is O(roots × entries), the same quadratic pattern #82
        // fixed for `deselect_root` itself.
        self.deselected_roots.retain(|root| {
            prefix_range_nonempty(&self.entries, root)
                || prefix_range_nonempty(&self.withheld, root)
        });
    }

    /// Deselects a whole allowlist root (#38, story 13): every currently-active Entry whose
    /// `relative_path` is under `root` moves to `withheld` in one action, and `root` is
    /// recorded in `deselected_roots` (so the exclusion — and its size — survives without
    /// listing each expanded file). This is how a heavy directory Entry (KDE Plasma's
    /// `icons/`/`wallpapers/`) is dropped as a single toggleable unit rather than by unchecking
    /// tens of thousands of files. A no-op that records nothing if no known Entry (active or
    /// withheld) falls under `root`, so a phantom root never lingers in the persisted set.
    ///
    /// #82: the whole active Subtree under `root` is lifted out via [`split_prefix_range`] —
    /// O(log n + k) for the `k` Entries it contains — instead of partitioning every Entry in
    /// the Selection. This is the fix for the Category-toggle freeze: `crates/tui`'s
    /// `set_category` deselect branch calls this once per Entry `relative_path` in the
    /// Category, so what used to be O(entries) per call (O(paths × entries) overall) is now
    /// O(log n) per call when `k` is small (the common per-file case) and still bounded by the
    /// touched subtree's own size otherwise.
    pub fn deselect_root(&mut self, root: &Path) {
        let moved = split_prefix_range(&mut self.entries, root);
        if moved.is_empty() && !prefix_range_nonempty(&self.withheld, root) {
            // Nothing (active or withheld) falls under `root` — `moved` being empty already
            // means nothing was removed from `self.entries` above, so there is nothing to
            // undo before returning.
            return;
        }
        self.total_size_cache.set(None);
        for (path, entry) in moved {
            self.withheld.insert(path, entry);
        }
        self.deselected_roots.insert(root.to_path_buf());
    }

    /// Reverses `deselect_root`: clears `root`'s deselection and moves its withheld Entries
    /// back into the active Selection — except any Entry still excluded by another origin: an
    /// individual per-file deselection (`deselected`) or a *different* root still deselected
    /// and also covering it. A no-op if `root` wasn't deselected.
    ///
    /// #82: the withheld Subtree under `root` is lifted out via [`split_prefix_range`] (same
    /// O(log n + k) shape as `deselect_root`) rather than partitioning every withheld Entry;
    /// each of the `k` candidates then gets its own O(log n) reactivation check.
    pub fn reselect_root(&mut self, root: &Path) {
        self.deselected_roots.remove(root);

        let candidates = split_prefix_range(&mut self.withheld, root);
        for (path, entry) in candidates {
            let reactivatable =
                !self.deselected.contains(&path) && !self.is_under_deselected_root(&path);
            if reactivatable {
                self.entries.insert(path, entry);
                self.total_size_cache.set(None);
            } else {
                self.withheld.insert(path, entry);
            }
        }
    }

    /// Whether `root` is currently deselected as a whole unit (#38) — the exact-match query a
    /// Drill-down UI uses to render a root's checkbox, distinct from `is_under_deselected_root`
    /// (which asks whether an arbitrary Entry path falls under *some* deselected root).
    pub fn is_root_deselected(&self, root: &Path) -> bool {
        self.deselected_roots.contains(root)
    }

    /// The [`DirectoryState`] of the directory node at `prefix` — the single source of truth
    /// for the recursive Drill-down explorer's `[x]`/`[~]`/`[ ]` tri-state (#48 / ADR-0006).
    /// Derived live from the active/withheld split, so it agrees with `deselect_root`/
    /// `reselect_root` by construction: a Subtree deselection moves every Entry under `prefix`
    /// into `withheld`, which reads back as `None`, and re-selecting them all reads back as
    /// `All`, with no stored tri-state to keep in sync. `prefix` is a relative path matched
    /// component-wise (matching `is_under_deselected_root`), so `.local/share/icons` covers
    /// `.local/share/icons/hicolor/x.svg` but not a sibling `.local/share/icons-backup`. A
    /// `prefix` covering no known Entry (active or withheld) reads as `None` — nothing there
    /// is selected; in practice the explorer only queries prefixes drawn from real Entry
    /// paths, so that is the edge, not the norm.
    ///
    /// #82: each check below is an O(log n) sorted-key range existence test
    /// ([`prefix_range_nonempty`]) rather than an O(n) scan of `entries`/`withheld`. #83:
    /// `crates/tui`'s `explorer_rows` no longer calls this once per visible row — it derives the
    /// same tri-state directly while it range-scans the breadcrumb Subtree
    /// ([`Self::active_entries_under`]/[`Self::withheld_entries_under`]), so this method stays
    /// the source-of-truth definition (and what the tests below and the round-trip seam pin)
    /// without being on that hot path at all.
    pub fn directory_state(&self, prefix: &Path) -> DirectoryState {
        let any_active = prefix_range_nonempty(&self.entries, prefix);
        let any_withheld = prefix_range_nonempty(&self.withheld, prefix);

        match (any_active, any_withheld) {
            (true, false) => DirectoryState::All,
            (true, true) => DirectoryState::Partial,
            (false, _) => DirectoryState::None,
        }
    }

    /// The active Entries (in the Selection) whose `relative_path` falls under `prefix`
    /// (`prefix` itself included) — a Subtree range scan (#82's range machinery), `O(log n +
    /// k)` for the `k` Entries actually under `prefix`.
    ///
    /// #83: this is what lets the recursive Drill-down explorer's `explorer_rows`
    /// (`crates/tui`) group the current breadcrumb's rows from just that Subtree, instead of
    /// filtering every Entry in the whole Selection (`entries_in_category`, `O(n)`) and testing
    /// each candidate's ancestry — so a render stays `O(k)` in the visible Subtree even when a
    /// sibling directory (another icon theme, say) or another Category sharing a path ancestor
    /// (e.g. `.config`) holds tens of thousands of Entries nothing on screen needs to touch.
    /// `tui` still filters the result by Category itself (an `O(k)` field comparison per
    /// candidate) since a `BTreeMap` keyed by `relative_path` alone can't range-scan on Category
    /// too — this method's job is bounding `k` to the breadcrumb's own Subtree, not to one
    /// Category, which the path-prefix range can't distinguish on its own.
    pub fn active_entries_under(&self, prefix: &Path) -> impl Iterator<Item = &Entry> {
        prefix_range(&self.entries, prefix).map(|(_, entry)| entry)
    }

    /// The withheld counterpart of [`Self::active_entries_under`]: Entries under `prefix`
    /// currently excluded (per-file or Subtree Drill-down deselection), so the explorer can
    /// still list them — unchecked, re-selectable — rather than only ever seeing what's active.
    pub fn withheld_entries_under(&self, prefix: &Path) -> impl Iterator<Item = &Entry> {
        prefix_range(&self.withheld, prefix).map(|(_, entry)| entry)
    }

    /// The relative paths of every currently-deselected Entry — the plain, storable shape
    /// #34 (Profile persistence) needs to remember per-file Drill-down deselections across runs
    /// (CONTEXT.md's "Drill-down": "Deselections persist in the Profile").
    pub fn deselected_paths(&self) -> &BTreeSet<PathBuf> {
        &self.deselected
    }

    /// The allowlist roots currently deselected as whole units (#38) — the storable shape the
    /// Profile persists alongside `deselected_paths`, one root path instead of the many files
    /// it expands to.
    pub fn deselected_roots(&self) -> &BTreeSet<PathBuf> {
        &self.deselected_roots
    }

    /// Every Entry belonging to `category` — active or withheld — for a Drill-down UI to
    /// list with per-Entry checkbox state (`is_deselected`), independently of whether the
    /// Category itself is checked.
    pub fn entries_in_category(&self, category: &str) -> Vec<&Entry> {
        self.entries
            .values()
            .chain(self.withheld.values())
            .filter(|e| e.category == category)
            .collect()
    }

    /// The absolute, on-disk paths of every selected Entry.
    pub fn selected_paths(&self) -> Vec<&Path> {
        self.entries
            .values()
            .map(|e| e.absolute_path.as_path())
            .collect()
    }

    /// The sum of every selected Entry's current on-disk size in bytes.
    ///
    /// WHERE/REVIEW's render paths and `RunPlan::new` call this on every render, and it used to
    /// re-`stat` every active Entry from disk each time even when the Selection hadn't changed
    /// since the last call — the common case for a screen that's just redrawing. The result is
    /// now cached in `total_size_cache` (a `Cell`, so this can stay `&self`) and only recomputed
    /// after a miss; every Selection-mutating method above invalidates it. This does *not* make
    /// the total live against the filesystem between calls — a file growing/shrinking with no
    /// intervening Selection mutation is still not reflected until something invalidates the
    /// cache, same as it wasn't this method's concern before caching existed.
    pub fn total_size(&self) -> u64 {
        if let Some(cached) = self.total_size_cache.get() {
            return cached;
        }
        let total = self
            .entries
            .values()
            .filter_map(|e| std::fs::metadata(&e.absolute_path).ok())
            .map(|m| m.len())
            .sum();
        self.total_size_cache.set(Some(total));
        total
    }

    /// Whether the Selection is empty (story 42: an empty Selection should bounce rather
    /// than proceed — the bounce itself is `tui` UX, this is the predicate it checks).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether Auto-insist requires whole-archive encryption for this Selection (stories
    /// 15/17: any Sensitive Entry — Category-level, per-entry tag, or Safety-net — forces it
    /// on). A thin delegate to `crypto::evaluate`, which also exposes exactly which
    /// Categories/Entries triggered it (story 18) for a TUI to show before prompting for a
    /// passphrase.
    pub fn encryption_required(&self) -> bool {
        crypto::evaluate(self).encryption_required
    }

    /// Human-readable warnings derived from the current Selection. Today this is only the
    /// User-data size warning (story 4: ">1GB"); the live running-total *display* as
    /// Categories are toggled is `tui`'s job (#30) — this is the pure fact it would render.
    pub fn warnings(&self) -> Vec<String> {
        let user_data_size: u64 = self
            .entries
            .values()
            .filter(|e| e.category == catalog::USER_DATA.name)
            .filter_map(|e| std::fs::metadata(&e.absolute_path).ok())
            .map(|m| m.len())
            .sum();

        let mut warnings = Vec::new();
        if user_data_size > USER_DATA_WARNING_THRESHOLD_BYTES {
            // Byte counts stay well under 2^52, so u64→f64 loses no precision here.
            #[allow(clippy::cast_precision_loss)]
            let gigabytes = user_data_size as f64 / USER_DATA_WARNING_THRESHOLD_BYTES as f64;
            warnings.push(format!(
                "User data selection is {gigabytes:.2} GB — this Category is large and default-OFF for a reason"
            ));
        }
        warnings
    }
}

/// The exclusive upper bound of the sorted-key range holding `prefix` itself and every path
/// with `prefix` as a component-wise prefix (CONTEXT.md's Subtree deselection) — `None` when
/// `prefix` has no components, meaning the range is the whole map.
///
/// Built by appending a NUL byte (`0x00`, never a legal byte in a Unix path component) to
/// `prefix`'s final component. The bumped component always sorts strictly after the
/// unmodified component every real descendant shares with `prefix` at that same position —
/// so it never clips a descendant, however deep — and strictly before any sibling whose
/// component at that position diverges and is lexicographically greater — so it never spills
/// into the next subtree. Both properties are decided at this one component regardless of a
/// descendant's remaining depth (`PathBuf`'s `Ord` compares components left-to-right, so once
/// one component differs, nothing after it matters), which is the load-bearing fact that lets
/// one bumped key stand in as the upper bound of an entire Subtree instead of the biggest key
/// actually inside it (which the caller doesn't know without the linear scan this exists to
/// avoid). See ADR-0006's dated addendum for the full argument.
fn prefix_range_end(prefix: &Path) -> Option<PathBuf> {
    let mut components: Vec<OsString> = prefix
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let mut bumped_last = components.pop()?.into_vec();
    bumped_last.push(0);
    components.push(OsString::from_vec(bumped_last));

    let mut bumped = PathBuf::new();
    for component in components {
        bumped.push(component);
    }
    Some(bumped)
}

/// The sorted-key range of `map` holding `prefix` and its whole Subtree — `prefix_range_end`'s
/// bound turned into an actual `BTreeMap` range lookup, `O(log n)` to position. Shared by
/// every Subtree query (`directory_state`, `deselect_root`, `reselect_root`, and
/// `deselect_category`'s `deselected_roots` cleanup) so the range-bound trick above has one
/// place to be correct.
fn prefix_range<'m>(map: &'m BTreeMap<PathBuf, Entry>, prefix: &Path) -> Range<'m, PathBuf, Entry> {
    match prefix_range_end(prefix) {
        Some(upper) => {
            map.range::<Path, _>((Bound::Included(prefix), Bound::Excluded(upper.as_path())))
        }
        None => map.range::<Path, _>((Bound::Included(prefix), Bound::Unbounded)),
    }
}

/// Whether `map` holds any Entry at or under `prefix` — an `O(log n)` existence check (stops
/// at the first match) rather than a full scan, the basis of `directory_state`'s tri-state and
/// the `deselect_root`/`deselect_category` no-op guards.
fn prefix_range_nonempty(map: &BTreeMap<PathBuf, Entry>, prefix: &Path) -> bool {
    prefix_range(map, prefix).next().is_some()
}

/// Removes `prefix`'s whole Subtree (itself plus every descendant) from `map` and returns
/// exactly the removed range as its own map, keyed the same way — `O(log n + k)` for the `k`
/// Entries actually under `prefix`, not a `retain`/partition over every Entry in `map` — the
/// core fix behind issue #82's Category-toggle freeze.
///
/// Deliberately *not* `BTreeMap::split_off` (which would seem the obvious tool): splitting at
/// `prefix` and again at the Subtree's upper bound isolates the target range, but stitching
/// the untouched remainder back onto `map` needs `BTreeMap::append`, and `append` is linear in
/// the combined size of both maps being merged — not the `O(log n)` its single-split cost
/// suggests. A single deep leaf's remainder is most of `map`, so that merge alone would put
/// the O(paths × entries) freeze right back for the common one-leaf-at-a-time call pattern
/// (`crates/tui`'s `set_category`). Collecting the range's keys read-only
/// ([`prefix_range`], `O(log n + k)`) and removing each by key (`O(log n)` per key, no
/// merge-back) avoids it.
fn split_prefix_range(
    map: &mut BTreeMap<PathBuf, Entry>,
    prefix: &Path,
) -> BTreeMap<PathBuf, Entry> {
    let keys: Vec<PathBuf> = prefix_range(map, prefix)
        .map(|(path, _)| path.clone())
        .collect();
    keys.into_iter()
        .filter_map(|path| {
            let entry = map.remove(&path)?;
            Some((path, entry))
        })
        .collect()
}

/// Builds an [`Entry`] for a Custom path — the one part of a Selection `scan::scan_taxonomy`
/// never re-discovers (the Custom-paths Category's allowlist is empty by design). Re-rooting
/// mirrors `scan`'s: a path under `home` is [`EntryRoot::Home`] with a home-relative path;
/// anything else is [`EntryRoot::Absolute`] with the leading `/` stripped for its
/// Archive-internal path. Sensitivity is re-derived from the Safety-net scan so a Custom path
/// pointing at a key file still forces encryption.
fn custom_entry(absolute_path: &Path, home: &Path) -> Entry {
    let (root, relative_path) = match absolute_path.strip_prefix(home) {
        Ok(relative) => (EntryRoot::Home, relative.to_path_buf()),
        Err(_) => (
            EntryRoot::Absolute,
            absolute_path
                .strip_prefix("/")
                .unwrap_or(absolute_path)
                .to_path_buf(),
        ),
    };
    Entry {
        absolute_path: absolute_path.to_path_buf(),
        relative_path,
        root,
        // A Custom path can point at a symlink (common with dotfile managers) — classify it
        // by lstat so `archive::build` preserves the link rather than storing its target's
        // bytes (issue #36), rather than assuming a regular file.
        entry_type: EntryType::classify(absolute_path),
        category: catalog::CUSTOM_PATHS.name,
        sensitive: scan::is_safety_net_match(absolute_path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::CategoryScan;
    use std::path::PathBuf;

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/tmp/does-not-need-to-exist/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive,
        }
    }

    #[test]
    fn from_smart_defaults_keeps_only_smart_default_categories() {
        let scanned = vec![
            CategoryScan {
                category: catalog::SSH,
                entries: vec![entry(catalog::SSH.name, true, "id_ed25519")],
            },
            CategoryScan {
                category: catalog::USER_DATA,
                entries: vec![entry(
                    catalog::USER_DATA.name,
                    false,
                    "Documents/report.pdf",
                )],
            },
        ];

        let selection = SelectionModel::from_smart_defaults(&scanned);

        // SSH defaults ON (story 3) so its Entry is kept; User data defaults OFF (story 4)
        // so its Entry must not appear even though it was present in the scan.
        assert_eq!(selection.entries().len(), 1);
        assert_eq!(selection.entries()[0].category, catalog::SSH.name);
    }

    #[test]
    fn warnings_is_empty_below_the_user_data_threshold() {
        let selection = SelectionModel::new(vec![entry(
            catalog::USER_DATA.name,
            false,
            "Documents/small.txt",
        )]);
        assert!(selection.warnings().is_empty());
    }

    #[test]
    fn warnings_flags_user_data_over_one_gigabyte() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let big_file = tmp.path().join("Documents/huge.bin");
        std::fs::create_dir_all(big_file.parent().unwrap()).expect("create dir");
        // A sparse file: reports the requested length via metadata without actually using
        // that much disk, which is all `warnings()` reads.
        let file = std::fs::File::create(&big_file).expect("create big file");
        file.set_len(USER_DATA_WARNING_THRESHOLD_BYTES + 1)
            .expect("set_len");

        let selection = SelectionModel::new(vec![Entry {
            absolute_path: big_file,
            relative_path: PathBuf::from("Documents/huge.bin"),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category: catalog::USER_DATA.name,
            sensitive: false,
        }]);

        let warnings = selection.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("User data"));
    }

    #[test]
    fn encryption_required_is_false_with_no_sensitive_entries() {
        let selection = SelectionModel::new(vec![entry(catalog::SHELL.name, false, "aliases.zsh")]);
        assert!(!selection.encryption_required());
    }

    #[test]
    fn encryption_required_is_true_with_a_sensitive_entry() {
        let selection = SelectionModel::new(vec![entry(catalog::SSH.name, true, "id_ed25519")]);
        assert!(selection.encryption_required());
    }

    /// Builds a real on-disk Entry (so `total_size` has something to `fs::metadata` against)
    /// of `len` bytes under `tmp`, at `relative_path`.
    fn entry_with_size(tmp: &Path, relative_path: &str, len: u64) -> Entry {
        let absolute_path = tmp.join(relative_path);
        std::fs::create_dir_all(absolute_path.parent().unwrap()).expect("create parent dir");
        let file = std::fs::File::create(&absolute_path).expect("create file");
        file.set_len(len).expect("set_len");
        Entry {
            absolute_path,
            relative_path: PathBuf::from(relative_path),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category: catalog::KDE_PLASMA.name,
            sensitive: false,
        }
    }

    #[test]
    fn deselect_entry_excludes_it_from_entries_selected_paths_and_total_size() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let heavy_icon = entry_with_size(tmp.path(), ".local/share/icons/heavy.svg", 200);
        let mut selection = SelectionModel::new(vec![kwinrc, heavy_icon]);

        selection.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        // Deselecting one Entry within KDE Plasma must not touch the Category-level
        // checkbox — the rest of the Category (kwinrc here) stays selected (AC2).
        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection.entries()[0].relative_path,
            Path::new(".config/kwinrc")
        );
        assert_eq!(
            selection.selected_paths(),
            vec![tmp.path().join(".config/kwinrc")]
        );
        assert_eq!(selection.total_size(), 100);
    }

    #[test]
    fn total_size_cache_hit_reuses_prior_value_across_an_out_of_band_disk_mutation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let absolute_path = entry.absolute_path.clone();
        let selection = SelectionModel::new(vec![entry]);

        assert_eq!(selection.total_size(), 100);

        // Out-of-band disk mutation with no intervening Selection mutation: a cache hit must
        // keep returning the value computed on the first call rather than re-stat.
        std::fs::File::create(&absolute_path)
            .expect("truncate file")
            .set_len(999)
            .expect("set_len");

        assert_eq!(selection.total_size(), 100);
    }

    #[test]
    fn total_size_cache_miss_recomputes_after_a_selection_mutation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let heavy_icon = entry_with_size(tmp.path(), ".local/share/icons/heavy.svg", 200);
        let mut selection = SelectionModel::new(vec![kwinrc, heavy_icon]);

        assert_eq!(selection.total_size(), 300);

        // A Selection mutation must invalidate the cache so the next call recomputes rather
        // than keeps returning the pre-mutation total.
        selection.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        assert_eq!(selection.total_size(), 100);
    }

    #[test]
    fn deselect_entry_is_recorded_in_is_deselected_and_deselected_paths() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/wallpapers/heavy.jpg",
            ),
        ]);

        selection.deselect_entry(Path::new(".local/share/wallpapers/heavy.jpg"));

        assert!(selection.is_deselected(Path::new(".local/share/wallpapers/heavy.jpg")));
        assert!(!selection.is_deselected(Path::new(".config/kwinrc")));
        assert_eq!(
            selection.deselected_paths(),
            &std::collections::BTreeSet::from([PathBuf::from(".local/share/wallpapers/heavy.jpg")])
        );
    }

    #[test]
    fn deselect_entry_is_a_no_op_for_a_path_not_in_the_selection() {
        let mut selection = SelectionModel::new(vec![entry(
            catalog::KDE_PLASMA.name,
            false,
            ".config/kwinrc",
        )]);

        selection.deselect_entry(Path::new("nonexistent"));

        assert_eq!(selection.entries().len(), 1);
        assert!(selection.deselected_paths().is_empty());
    }

    #[test]
    fn reselect_entry_restores_a_previously_deselected_entry() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/heavy.svg",
            ),
        ]);
        let icon_path = Path::new(".local/share/icons/heavy.svg");

        selection.deselect_entry(icon_path);
        assert_eq!(selection.entries().len(), 1);

        selection.reselect_entry(icon_path);

        assert_eq!(selection.entries().len(), 2);
        assert!(!selection.is_deselected(icon_path));
        assert!(selection.deselected_paths().is_empty());
    }

    #[test]
    fn reselect_entry_is_a_no_op_when_the_path_was_never_withheld() {
        let mut selection = SelectionModel::new(vec![entry(
            catalog::KDE_PLASMA.name,
            false,
            ".config/kwinrc",
        )]);

        selection.reselect_entry(Path::new(".config/kwinrc"));

        assert_eq!(selection.entries().len(), 1);
    }

    #[test]
    fn entries_in_category_lists_both_active_and_withheld_entries() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/heavy.svg",
            ),
            entry(catalog::SHELL.name, false, "aliases.zsh"),
        ]);
        selection.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        // Drill-down needs to list every Entry in the Category — including the withheld
        // one, so a UI can show it unchecked and let the user toggle it back on — not just
        // whatever remains actively selected.
        let plasma_entries = selection.entries_in_category(catalog::KDE_PLASMA.name);
        let plasma_relative_paths: std::collections::BTreeSet<&Path> = plasma_entries
            .iter()
            .map(|e| e.relative_path.as_path())
            .collect();
        assert_eq!(
            plasma_relative_paths,
            std::collections::BTreeSet::from([
                Path::new(".config/kwinrc"),
                Path::new(".local/share/icons/heavy.svg"),
            ])
        );
    }

    #[test]
    fn deselecting_the_only_sensitive_entry_lifts_encryption_required() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::SHELL.name, false, "aliases.zsh"),
            entry(catalog::SSH.name, true, "id_ed25519"),
        ]);
        assert!(selection.encryption_required());

        // Auto-insist (#22) must recompute from whatever is still actively selected — a
        // withheld sensitive Entry can no longer be what lands in the Archive, so it must
        // stop being what justifies forcing encryption on.
        selection.deselect_entry(Path::new("id_ed25519"));

        assert!(!selection.encryption_required());
    }

    #[test]
    fn deselecting_the_heavy_user_data_entry_clears_the_size_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let huge = entry_with_size(
            tmp.path(),
            "Documents/huge.bin",
            USER_DATA_WARNING_THRESHOLD_BYTES + 1,
        );
        let huge = Entry {
            category: catalog::USER_DATA.name,
            ..huge
        };
        let mut selection = SelectionModel::new(vec![huge]);
        assert!(!selection.warnings().is_empty());

        // `warnings()` sums size over `entries` only (module doc's #21 section), so
        // withholding the one oversized Entry must make the warning disappear rather than
        // linger on a Selection that no longer actually contains it.
        selection.deselect_entry(Path::new("Documents/huge.bin"));

        assert!(selection.warnings().is_empty());
    }

    #[test]
    fn select_category_activates_every_entry_of_a_category_never_seen_before() {
        // Simulates the WHAT screen (#30) toggling ON a Category whose Smart default was
        // OFF (e.g. User data): the model starts with zero knowledge of its Entries at all.
        let mut selection =
            SelectionModel::new(vec![entry(catalog::SHELL.name, false, "aliases.zsh")]);
        let scan = CategoryScan {
            category: catalog::USER_DATA,
            entries: vec![
                entry(catalog::USER_DATA.name, false, "Documents/report.pdf"),
                entry(catalog::USER_DATA.name, false, "Downloads/file.zip"),
            ],
        };

        selection.select_category(&scan);

        assert_eq!(selection.entries().len(), 3);
        assert!(selection
            .entries()
            .iter()
            .any(|e| e.relative_path == Path::new("Documents/report.pdf")));
        assert!(selection
            .entries()
            .iter()
            .any(|e| e.relative_path == Path::new("Downloads/file.zip")));
    }

    #[test]
    fn select_category_is_idempotent_and_does_not_duplicate_already_active_entries() {
        let entries = vec![entry(catalog::SHELL.name, false, "aliases.zsh")];
        let scan = CategoryScan {
            category: catalog::SHELL,
            entries: entries.clone(),
        };
        let mut selection = SelectionModel::new(entries);

        selection.select_category(&scan);

        assert_eq!(selection.entries().len(), 1);
    }

    #[test]
    fn select_category_reselects_a_previously_withheld_entry_rather_than_duplicating_it() {
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![
                entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
                entry(
                    catalog::KDE_PLASMA.name,
                    false,
                    ".local/share/icons/heavy.svg",
                ),
            ],
        };
        let mut selection = SelectionModel::new(scan.entries.clone());
        selection.deselect_entry(Path::new(".local/share/icons/heavy.svg"));
        assert_eq!(selection.entries().len(), 1);

        // A Drill-down UI calls `select_category` unconditionally before listing Entries
        // (module doc) — it must restore the withheld one to active, not push a duplicate.
        selection.select_category(&scan);

        assert_eq!(selection.entries().len(), 2);
        assert!(!selection.is_deselected(Path::new(".local/share/icons/heavy.svg")));
    }

    #[test]
    fn deselect_category_removes_both_active_and_withheld_entries_for_that_category_only() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/heavy.svg",
            ),
            entry(catalog::SHELL.name, false, "aliases.zsh"),
        ]);
        selection.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        selection.deselect_category(catalog::KDE_PLASMA.name);

        assert_eq!(selection.entries().len(), 1);
        assert_eq!(selection.entries()[0].category, catalog::SHELL.name);
        // The Category's own Drill-down deselection bookkeeping must be cleared too, not
        // left lingering as a stale withheld path with no way back to active.
        assert!(selection.deselected_paths().is_empty());
        assert!(selection
            .entries_in_category(catalog::KDE_PLASMA.name)
            .is_empty());
    }

    #[test]
    fn toggling_a_category_off_then_on_restores_its_full_scanned_entry_set() {
        let scan = CategoryScan {
            category: catalog::SHELL,
            entries: vec![entry(catalog::SHELL.name, false, "aliases.zsh")],
        };
        let mut selection = SelectionModel::new(scan.entries.clone());

        selection.deselect_category(catalog::SHELL.name);
        assert!(selection.is_empty());

        selection.select_category(&scan);
        assert_eq!(selection.entries().len(), 1);
    }

    #[test]
    fn select_category_stays_fast_for_a_realistically_large_category() {
        // Regression test for a real bug found during #30's manual verification: KDE
        // Plasma's icon theme alone produced ~40,000 real Entries on the author's account,
        // and the original `Vec::iter().any(...)`-per-Entry membership check made loading
        // that many Entries an O(n^2) scan — measured at ~88s in practice. This pins the fix
        // (an O(log n) `BTreeMap::contains_key` lookup per Entry, #82) down with a generous,
        // non-flaky time budget: comfortably above what the fixed version needs (single-digit
        // milliseconds), but far below what the O(n^2) version would take at this size.
        let entries: Vec<Entry> = (0..20_000)
            .map(|i| entry(catalog::KDE_PLASMA.name, false, &format!("icon{i}.svg")))
            .collect();
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries,
        };
        let mut selection = SelectionModel::new(Vec::new());

        let started = std::time::Instant::now();
        selection.select_category(&scan);
        let elapsed = started.elapsed();

        assert_eq!(selection.entries().len(), 20_000);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "select_category took {elapsed:?} for 20,000 Entries — the O(n^2) regression is back"
        );
    }

    #[test]
    fn deselecting_a_realistically_large_category_one_leaf_at_a_time_stays_fast() {
        // Regression test for issue #82: `crates/tui`'s `set_category` deselect branch
        // (the WHAT screen's Category checkbox turned off) calls `deselect_root` once per
        // Entry `relative_path` in the Category, not once for the whole Category — so the
        // bug wasn't `deselect_root` being slow on its own, it was O(entries) *per call*,
        // compounding into O(paths × entries) across the loop. This is what froze toggling
        // off KDE Plasma's ~40,000-file icon theme in practice. Pinning `select_category`
        // alone (the test above, fixed back in #30) doesn't cover this: that bug is entirely
        // on the deselect side.
        let leaf_paths: Vec<PathBuf> = (0..40_000)
            .map(|i| PathBuf::from(format!("icon{i}.svg")))
            .collect();
        let entries: Vec<Entry> = leaf_paths
            .iter()
            .map(|path| {
                entry(
                    catalog::KDE_PLASMA.name,
                    false,
                    path.to_str().expect("ascii test path"),
                )
            })
            .collect();
        let mut selection = SelectionModel::new(entries);

        let started = std::time::Instant::now();
        for path in &leaf_paths {
            selection.deselect_root(path);
        }
        let elapsed = started.elapsed();

        assert!(selection.is_empty());
        assert_eq!(selection.deselected_roots().len(), 40_000);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "deselecting 40,000 Entries one leaf at a time took {elapsed:?} — the O(paths × \
             entries) Category-toggle freeze (#82) is back"
        );
    }

    #[test]
    fn deselect_root_withholds_the_whole_subtree_from_entries_and_total_size_in_one_action() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let icon_a = entry_with_size(tmp.path(), ".local/share/icons/hicolor/a.svg", 200);
        let icon_b = entry_with_size(tmp.path(), ".local/share/icons/hicolor/b.svg", 300);
        let mut selection = SelectionModel::new(vec![kwinrc, icon_a, icon_b]);

        // Story 13: one toggle drops the whole heavy icon theme, not each expanded file.
        selection.deselect_root(Path::new(".local/share/icons"));

        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection.entries()[0].relative_path,
            Path::new(".config/kwinrc")
        );
        // AC2: the running-total size drops the whole subtree, not just one file.
        assert_eq!(selection.total_size(), 100);
        assert!(selection.is_root_deselected(Path::new(".local/share/icons")));
        assert_eq!(
            selection.deselected_roots(),
            &BTreeSet::from([PathBuf::from(".local/share/icons")])
        );
        // The per-file registry stays empty — a root deselection records one root, not the
        // many files under it (the whole point of the storable shape #38 persists).
        assert!(selection.deselected_paths().is_empty());
    }

    #[test]
    fn deselect_root_matches_on_path_components_not_a_string_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let icon = entry_with_size(tmp.path(), ".local/share/icons/a.svg", 10);
        let sibling = entry_with_size(tmp.path(), ".local/share/icons-backup/b.svg", 20);
        let mut selection = SelectionModel::new(vec![icon, sibling]);

        selection.deselect_root(Path::new(".local/share/icons"));

        // `icons-backup` is a sibling, not under `icons/`, so it must survive.
        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection.entries()[0].relative_path,
            Path::new(".local/share/icons-backup/b.svg")
        );
    }

    #[test]
    fn deselect_root_covering_a_single_file_root_behaves_like_a_per_entry_deselection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let plasmarc = entry_with_size(tmp.path(), ".config/plasmarc", 50);
        let mut selection = SelectionModel::new(vec![kwinrc, plasmarc]);

        // A single-file allowlist root (`.config/kwinrc`) is its own unit: deselecting it is
        // the same as excluding that one Entry, so file-roots keep individual control.
        selection.deselect_root(Path::new(".config/kwinrc"));

        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection.entries()[0].relative_path,
            Path::new(".config/plasmarc")
        );
        assert!(selection.is_root_deselected(Path::new(".config/kwinrc")));
    }

    #[test]
    fn deselect_root_records_nothing_when_no_entry_falls_under_it() {
        let mut selection = SelectionModel::new(vec![entry(
            catalog::KDE_PLASMA.name,
            false,
            ".config/kwinrc",
        )]);

        selection.deselect_root(Path::new(".local/share/wallpapers"));

        assert_eq!(selection.entries().len(), 1);
        assert!(selection.deselected_roots().is_empty());
    }

    #[test]
    fn reselect_root_restores_exactly_the_previously_root_deselected_subtree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let icon_a = entry_with_size(tmp.path(), ".local/share/icons/a.svg", 200);
        let icon_b = entry_with_size(tmp.path(), ".local/share/icons/b.svg", 300);
        let mut selection = SelectionModel::new(vec![kwinrc, icon_a, icon_b]);
        let root = Path::new(".local/share/icons");

        selection.deselect_root(root);
        assert_eq!(selection.entries().len(), 1);

        selection.reselect_root(root);

        assert_eq!(selection.entries().len(), 3);
        assert!(!selection.is_root_deselected(root));
        assert!(selection.deselected_roots().is_empty());
        assert_eq!(selection.total_size(), 600);
    }

    #[test]
    fn reselect_root_keeps_an_entry_that_is_also_individually_deselected_withheld() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/a.svg"),
            entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/b.svg"),
        ]);
        let root = Path::new(".local/share/icons");

        // Belt-and-suspenders: one icon dropped individually, then the whole root dropped.
        selection.deselect_entry(Path::new(".local/share/icons/a.svg"));
        selection.deselect_root(root);
        assert!(selection.entries().is_empty());

        // Lifting only the root must not resurrect the file the user also killed individually.
        selection.reselect_root(root);

        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection.entries()[0].relative_path,
            Path::new(".local/share/icons/b.svg")
        );
        assert!(selection.is_deselected(Path::new(".local/share/icons/a.svg")));
    }

    #[test]
    fn select_category_keeps_a_root_deselected_subtree_withheld() {
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![
                entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
                entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/a.svg"),
                entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/b.svg"),
            ],
        };
        let mut selection = SelectionModel::new(scan.entries.clone());
        selection.deselect_root(Path::new(".local/share/icons"));
        assert_eq!(selection.entries().len(), 1);

        // A Drill-down UI calls `select_category` unconditionally before listing units — it
        // must not resurrect a root the user deselected (contrast a per-file deselection,
        // which the same call *does* lift, per the test below).
        selection.select_category(&scan);

        assert_eq!(selection.entries().len(), 1);
        assert!(selection.is_root_deselected(Path::new(".local/share/icons")));
    }

    #[test]
    fn deselect_category_forgets_its_root_deselections() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/a.svg"),
            entry(catalog::SHELL.name, false, "aliases.zsh"),
        ]);
        selection.deselect_root(Path::new(".local/share/icons"));
        assert_eq!(
            selection.deselected_roots(),
            &BTreeSet::from([PathBuf::from(".local/share/icons")])
        );

        selection.deselect_category(catalog::KDE_PLASMA.name);

        // The whole Category is gone, so its root deselection is orphaned and must be dropped
        // rather than lingering as a stale root pointing at nothing.
        assert!(selection.deselected_roots().is_empty());
        assert_eq!(selection.entries().len(), 1);
        assert_eq!(selection.entries()[0].category, catalog::SHELL.name);
    }

    #[test]
    fn deselecting_a_root_leaves_the_category_partial_not_fully_off() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), ".config/kwinrc", 100);
        let icon = entry_with_size(tmp.path(), ".local/share/icons/a.svg", 200);
        let mut selection = SelectionModel::new(vec![kwinrc, icon]);

        selection.deselect_root(Path::new(".local/share/icons"));

        // AC3: some-but-not-all deselected — the Category still has an active Entry, so it is
        // partial, never fully off. `entries_in_category` still surfaces every unit for re-toggling.
        assert!(!selection.is_empty());
        assert_eq!(selection.entries().len(), 1);
        assert_eq!(
            selection
                .entries_in_category(catalog::KDE_PLASMA.name)
                .len(),
            2
        );
    }

    #[test]
    fn add_custom_path_under_home_is_home_rooted() {
        let home = Path::new("/home/test");
        let mut selection = SelectionModel::new(Vec::new());

        selection.add_custom_path(Path::new("/home/test/projects/notes.md"), home);

        assert_eq!(selection.entries().len(), 1);
        let custom = &selection.entries()[0];
        assert_eq!(custom.category, catalog::CUSTOM_PATHS.name);
        assert_eq!(custom.root, EntryRoot::Home);
        assert_eq!(custom.relative_path, Path::new("projects/notes.md"));
        assert!(!custom.sensitive);
    }

    #[test]
    fn add_custom_path_outside_home_is_absolute_and_safety_net_stays_sensitive() {
        let home = Path::new("/home/test");
        let mut selection = SelectionModel::new(Vec::new());

        // A root-added path outside $HOME at a key file: absolute-rooted, and the Safety-net
        // scan re-derives its sensitivity so it still Auto-insists on encryption.
        selection.add_custom_path(Path::new("/etc/ssl/private/server.key"), home);

        let custom = &selection.entries()[0];
        assert_eq!(custom.root, EntryRoot::Absolute);
        assert_eq!(
            custom.relative_path,
            Path::new("etc/ssl/private/server.key")
        );
        assert!(custom.sensitive);
        assert!(selection.encryption_required());
    }

    #[test]
    fn add_custom_path_ignores_a_re_added_duplicate() {
        let home = Path::new("/home/test");
        let mut selection = SelectionModel::new(Vec::new());
        let path = Path::new("/home/test/projects/notes.md");

        selection.add_custom_path(path, home);
        selection.add_custom_path(path, home);

        assert_eq!(
            selection.entries().len(),
            1,
            "re-adding the same Custom path must not double-count it"
        );
    }

    #[test]
    fn re_adding_a_withheld_custom_path_reactivates_it() {
        // #106's folded-in dedupe fix: the guard now also consults `withheld`, and typing the
        // path again is an explicit "I want this backed up" that reverses the withhold rather
        // than either no-op'ing or inserting a duplicate Entry.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let file_path = home.join("notes.md");
        std::fs::write(&file_path, b"hello").expect("write file");
        let mut selection = SelectionModel::new(Vec::new());

        selection.add_custom_path(&file_path, home);
        selection.deselect_entry(Path::new("notes.md"));
        assert!(selection.entries().is_empty());

        selection.add_custom_path(&file_path, home);

        assert_eq!(selection.entries().len(), 1);
        assert_eq!(selection.entries()[0].relative_path, Path::new("notes.md"));
        assert!(!selection.is_deselected(Path::new("notes.md")));
        assert_eq!(
            selection
                .entries_in_category(catalog::CUSTOM_PATHS.name)
                .len(),
            1,
            "reactivating must not leave a second, duplicate Entry behind"
        );
    }

    #[test]
    fn remove_custom_path_drops_an_active_entry() {
        let mut selection =
            SelectionModel::new(vec![entry(catalog::CUSTOM_PATHS.name, false, "notes.md")]);

        selection.remove_custom_path(Path::new("notes.md"));

        assert!(selection.entries().is_empty());
        assert!(selection
            .entries_in_category(catalog::CUSTOM_PATHS.name)
            .is_empty());
    }

    #[test]
    fn remove_custom_path_drops_a_withheld_entry() {
        let mut selection =
            SelectionModel::new(vec![entry(catalog::CUSTOM_PATHS.name, false, "notes.md")]);
        selection.deselect_entry(Path::new("notes.md"));
        assert!(!selection
            .entries_in_category(catalog::CUSTOM_PATHS.name)
            .is_empty());

        selection.remove_custom_path(Path::new("notes.md"));

        assert!(selection
            .entries_in_category(catalog::CUSTOM_PATHS.name)
            .is_empty());
        assert!(selection.deselected_paths().is_empty());
    }

    #[test]
    fn remove_custom_path_prunes_an_orphaned_root() {
        let mut selection =
            SelectionModel::new(vec![entry(catalog::CUSTOM_PATHS.name, false, "myproj")]);
        selection.deselect_root(Path::new("myproj"));
        assert!(selection.is_root_deselected(Path::new("myproj")));

        selection.remove_custom_path(Path::new("myproj"));

        assert!(
            selection.deselected_roots().is_empty(),
            "a lone directory Custom path must prune its own Subtree deselection on removal"
        );
        assert!(selection
            .entries_in_category(catalog::CUSTOM_PATHS.name)
            .is_empty());
    }

    #[test]
    fn remove_custom_path_retains_a_still_covering_root() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::CUSTOM_PATHS.name, false, ".config"),
            entry(catalog::PROGRAM_CONFIGS.name, false, ".config/foo.conf"),
        ]);
        selection.deselect_root(Path::new(".config"));
        assert!(selection.is_root_deselected(Path::new(".config")));

        selection.remove_custom_path(Path::new(".config"));

        // The Custom path Entry itself is gone...
        assert!(selection
            .entries_in_category(catalog::CUSTOM_PATHS.name)
            .is_empty());
        // ...but the root deselection survives because a Programs Entry still falls under it,
        // leaving that Category exactly as the user last saw it: withheld, not silently
        // re-selected just because the Custom path that shared its prefix was cleared.
        assert!(selection.is_root_deselected(Path::new(".config")));
        assert_eq!(
            selection
                .entries_in_category(catalog::PROGRAM_CONFIGS.name)
                .len(),
            1
        );
        assert!(selection.entries().is_empty());
    }

    #[test]
    fn remove_custom_path_ignores_a_non_custom_entry() {
        let mut selection = SelectionModel::new(vec![entry(
            catalog::PROGRAM_CONFIGS.name,
            false,
            ".config/foo.conf",
        )]);

        selection.remove_custom_path(Path::new(".config/foo.conf"));

        assert_eq!(
            selection.entries().len(),
            1,
            "the Category guard must not let remove_custom_path delete another Category's Entry"
        );
        assert_eq!(
            selection.entries()[0].category,
            catalog::PROGRAM_CONFIGS.name
        );
    }

    /// A deep Drill-down tree (#48): several nested levels under one Category, so the
    /// intermediate-depth cases the alpha's shallow `.local/share/icons` fixtures never
    /// reach can be exercised.
    fn deep_icon_selection() -> SelectionModel {
        SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/hicolor/apps/firefox.svg",
            ),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/hicolor/apps/code.svg",
            ),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/hicolor/mimetypes/text.svg",
            ),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons/scalable/logo.svg",
            ),
        ])
    }

    #[test]
    fn directory_state_is_all_when_every_entry_under_the_prefix_is_active() {
        let selection = deep_icon_selection();

        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons")),
            DirectoryState::All
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor/apps")),
            DirectoryState::All
        );
    }

    #[test]
    fn directory_state_is_none_for_a_prefix_covering_no_known_entry() {
        let selection = deep_icon_selection();

        // An empty subtree holds nothing selected — the drill-disabled `—` node (#34).
        assert_eq!(
            selection.directory_state(Path::new(".local/share/wallpapers")),
            DirectoryState::None
        );
    }

    #[test]
    fn directory_state_agrees_with_deselect_root_at_an_intermediate_depth() {
        let mut selection = deep_icon_selection();
        let intermediate = Path::new(".local/share/icons/hicolor/apps");

        // Deselect a node several levels deep — the case the alpha's two-level fixtures never
        // reached. Prefix-wins: both `apps/*` Entries are withheld as one unit.
        selection.deselect_root(intermediate);

        assert!(selection.is_root_deselected(intermediate));
        assert_eq!(
            selection.deselected_roots(),
            &BTreeSet::from([intermediate.to_path_buf()])
        );
        // The deselected intermediate node reads None; every ancestor holding a still-active
        // sibling reads Partial; the untouched sibling subtrees stay All.
        assert_eq!(
            selection.directory_state(intermediate),
            DirectoryState::None
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor")),
            DirectoryState::Partial
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons")),
            DirectoryState::Partial
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor/mimetypes")),
            DirectoryState::All
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/scalable")),
            DirectoryState::All
        );
        assert_eq!(
            selection.directory_state(Path::new(".config")),
            DirectoryState::All
        );
        // No per-file marks recorded — an intermediate Subtree deselection stays one root.
        assert!(selection.deselected_paths().is_empty());
    }

    #[test]
    fn directory_state_reflects_a_nested_deselect_root_prefix_wins_over_the_whole_subtree() {
        let mut selection = deep_icon_selection();
        let hicolor = Path::new(".local/share/icons/hicolor");

        // Deselecting the intermediate `hicolor` node withholds every leaf beneath it (apps +
        // mimetypes) in one action, leaving only the `scalable` sibling active.
        selection.deselect_root(hicolor);

        assert_eq!(selection.directory_state(hicolor), DirectoryState::None);
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor/apps")),
            DirectoryState::None,
            "a descendant of a deselected intermediate node is also None"
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons")),
            DirectoryState::Partial
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/scalable")),
            DirectoryState::All
        );
    }

    #[test]
    fn directory_state_agrees_with_reselect_root_restoring_all() {
        let mut selection = deep_icon_selection();
        let intermediate = Path::new(".local/share/icons/hicolor/apps");

        selection.deselect_root(intermediate);
        selection.reselect_root(intermediate);

        // Reversing the intermediate deselection reads back as All everywhere it had read None.
        assert_eq!(selection.directory_state(intermediate), DirectoryState::All);
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor")),
            DirectoryState::All
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons")),
            DirectoryState::All
        );
    }

    #[test]
    fn directory_state_matches_on_path_components_not_a_string_prefix() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, false, ".local/share/icons/a.svg"),
            entry(
                catalog::KDE_PLASMA.name,
                false,
                ".local/share/icons-backup/b.svg",
            ),
        ]);

        selection.deselect_root(Path::new(".local/share/icons"));

        // The deselected `icons` subtree is None; its string-prefixed sibling `icons-backup`
        // is a different subtree and stays All (mirrors `deselect_root`'s component matching).
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons")),
            DirectoryState::None
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons-backup")),
            DirectoryState::All
        );
    }

    #[test]
    fn directory_state_reports_partial_from_a_per_file_deselection() {
        let mut selection = deep_icon_selection();

        // The tri-state tracks actual inclusion, so a per-file Drill-down deselection makes its
        // ancestor Partial just as a root deselection does — the query is not root-only.
        selection.deselect_entry(Path::new(".local/share/icons/hicolor/apps/firefox.svg"));

        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor/apps")),
            DirectoryState::Partial
        );
        assert_eq!(
            selection.directory_state(Path::new(".local/share/icons/hicolor/apps/firefox.svg")),
            DirectoryState::None,
            "the deselected file node itself reads None"
        );
    }

    #[test]
    fn active_entries_under_returns_only_the_subtree_not_the_whole_selection() {
        let selection = deep_icon_selection();

        let apps: BTreeSet<PathBuf> = selection
            .active_entries_under(Path::new(".local/share/icons/hicolor/apps"))
            .map(|entry| entry.relative_path.clone())
            .collect();

        assert_eq!(
            apps,
            BTreeSet::from([
                PathBuf::from(".local/share/icons/hicolor/apps/firefox.svg"),
                PathBuf::from(".local/share/icons/hicolor/apps/code.svg"),
            ]),
            "only the queried Subtree comes back, not sibling icons or the .config Category"
        );
    }

    #[test]
    fn withheld_entries_under_lists_a_deselected_subtree_active_entries_under_does_not() {
        let mut selection = deep_icon_selection();
        let apps = Path::new(".local/share/icons/hicolor/apps");
        selection.deselect_root(apps);

        assert_eq!(selection.active_entries_under(apps).count(), 0);
        let withheld: BTreeSet<PathBuf> = selection
            .withheld_entries_under(apps)
            .map(|entry| entry.relative_path.clone())
            .collect();
        assert_eq!(
            withheld,
            BTreeSet::from([
                PathBuf::from(".local/share/icons/hicolor/apps/firefox.svg"),
                PathBuf::from(".local/share/icons/hicolor/apps/code.svg"),
            ])
        );
    }

    #[test]
    fn active_entries_under_stays_fast_when_a_sibling_subtree_is_huge() {
        // Regression guard for #83: a naive `entries_in_category`-style scan touches every
        // Entry in the Selection regardless of which breadcrumb is being rendered, so a huge
        // *unrelated* sibling subtree (a second icon theme, say) would still cost the render —
        // even though nothing under it is on screen. The range scan must stay bounded to the
        // queried prefix's own size, not the Selection's.
        let mut entries: Vec<Entry> = (0..30_000)
            .map(|i| {
                entry(
                    catalog::KDE_PLASMA.name,
                    false,
                    &format!(".local/share/icons/huge-sibling-theme/icon{i}.svg"),
                )
            })
            .collect();
        entries.push(entry(
            catalog::KDE_PLASMA.name,
            false,
            ".local/share/icons/small/only.svg",
        ));
        let selection = SelectionModel::new(entries);

        let started = std::time::Instant::now();
        let small: Vec<&Entry> = selection
            .active_entries_under(Path::new(".local/share/icons/small"))
            .collect();
        let elapsed = started.elapsed();

        assert_eq!(small.len(), 1);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "active_entries_under took {elapsed:?} to fetch one Entry past a 30,000-Entry \
             sibling subtree — the O(n)-per-query regression is back"
        );
    }
}
