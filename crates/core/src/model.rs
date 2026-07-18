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
//! disappear for free, with no per-accessor filtering to keep in sync. `withheld` and
//! `deselected` exist side by side because they serve different callers: `withheld` keeps
//! the full `Entry` around so a Drill-down UI can re-list and reselect it; `deselected` is
//! the plain `BTreeSet<PathBuf>` of relative paths — the storable shape #34 (Profile
//! persistence) needs to remember deselections across runs (CONTEXT.md's "Profile").
//!
//! #22 scope: `encryption_required` delegates to `crypto::evaluate`'s Auto-insist policy —
//! see that module for which specific Categories/Entries triggered it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::catalog;
use crate::crypto;
use crate::scan::CategoryScan;

/// The size threshold (story 4: ">1GB") above which a User-data Selection earns a warning.
const USER_DATA_WARNING_THRESHOLD_BYTES: u64 = 1_000_000_000;

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

/// The user's chosen set of Entries for a run (CONTEXT.md's "Selection"). `core`'s
/// authoritative model of what will be backed up.
#[derive(Debug, Clone, Default)]
pub struct SelectionModel {
    /// The currently active Entries — see the module doc's #21 section for why every
    /// derived accessor reads only this field.
    entries: Vec<Entry>,
    /// Entries deselected via Drill-down, held here rather than dropped so
    /// `entries_in_category`/`reselect_entry` can still find them.
    withheld: Vec<Entry>,
    /// The relative paths of every currently-withheld Entry, kept in lockstep with
    /// `withheld` — the storable shape exposed by `deselected_paths` for #34 to persist.
    deselected: BTreeSet<PathBuf>,
}

impl SelectionModel {
    /// Builds a Selection from an already-scanned, already-selected set of Entries. Useful
    /// on its own for a single Category (as the round-trip seam's `shell_selection` helper
    /// does) or once Drill-down (#21) has finished toggling individual Entries within an
    /// otherwise Smart-defaulted Selection.
    pub fn new(entries: Vec<Entry>) -> Self {
        Self {
            entries,
            withheld: Vec::new(),
            deselected: BTreeSet::new(),
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

    /// The active (non-deselected) Entries, in scan order — what `archive::build` and the
    /// Manifest builder actually consume.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Deselects (Drill-down, #21) the Entry whose relative path is `relative_path`,
    /// independently of its Category's own checkbox: the Entry moves out of the active
    /// Selection (so `selected_paths`/`total_size`/`entries()` stop seeing it) into
    /// `withheld`, where `entries_in_category`/`reselect_entry` can still find it. A no-op
    /// if no active Entry matches `relative_path` — already deselected, or never part of
    /// the Selection to begin with.
    pub fn deselect_entry(&mut self, relative_path: &Path) {
        let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.relative_path == relative_path)
        else {
            return;
        };
        let entry = self.entries.remove(pos);
        self.deselected.insert(entry.relative_path.clone());
        self.withheld.push(entry);
    }

    /// Reverses `deselect_entry`: moves the Entry back into the active Selection. A no-op
    /// if `relative_path` isn't currently withheld.
    pub fn reselect_entry(&mut self, relative_path: &Path) {
        let Some(pos) = self
            .withheld
            .iter()
            .position(|e| e.relative_path == relative_path)
        else {
            return;
        };
        let entry = self.withheld.remove(pos);
        self.deselected.remove(&entry.relative_path);
        self.entries.push(entry);
    }

    /// Whether `relative_path` is currently deselected (withheld from the active
    /// Selection).
    pub fn is_deselected(&self, relative_path: &Path) -> bool {
        self.deselected.contains(relative_path)
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
    /// Membership is checked via a `BTreeSet` snapshot taken once up front, not a
    /// `Vec::iter().any(...)` per Entry: the naive per-Entry linear scan is O(current
    /// Selection size) each time, which turns loading a large Category into this model into
    /// an O(n²) scan — measured at ~88s for KDE Plasma's ~40,000-file icon theme (a real
    /// Category size, story 13) during #30's manual verification against a real account.
    pub fn select_category(&mut self, scan: &CategoryScan) {
        let active: BTreeSet<&Path> = self
            .entries
            .iter()
            .map(|e| e.relative_path.as_path())
            .collect();
        let withheld: BTreeSet<&Path> = self
            .withheld
            .iter()
            .map(|e| e.relative_path.as_path())
            .collect();

        let mut to_reselect = Vec::new();
        let mut to_push = Vec::new();
        for entry in &scan.entries {
            let path = entry.relative_path.as_path();
            if active.contains(path) {
                continue;
            }
            if withheld.contains(path) {
                to_reselect.push(entry.relative_path.clone());
            } else {
                to_push.push(entry.clone());
            }
        }

        for path in to_reselect {
            self.reselect_entry(&path);
        }
        self.entries.extend(to_push);
    }

    /// Category-level toggle OFF: removes every Entry belonging to `category_name` from the
    /// Selection entirely — both active and `withheld` — rather than only clearing active
    /// ones. Toggling back on (`select_category`) then re-adds the Category's full scanned
    /// Entry set fresh: the simplest checkbox semantics to reason about (an unchecked box
    /// carries no partial memory), even though it means a Drill-down deselection doesn't
    /// survive a full off/on cycle within the same run — an accepted simplification for the
    /// alpha's first Selection screen (#30); re-drilling after re-checking is cheap.
    pub fn deselect_category(&mut self, category_name: &str) {
        self.entries.retain(|e| e.category != category_name);

        let removed_withheld: Vec<PathBuf> = self
            .withheld
            .iter()
            .filter(|e| e.category == category_name)
            .map(|e| e.relative_path.clone())
            .collect();
        self.withheld.retain(|e| e.category != category_name);
        for path in removed_withheld {
            self.deselected.remove(&path);
        }
    }

    /// The relative paths of every currently-deselected Entry — the plain, storable shape
    /// #34 (Profile persistence) needs to remember Drill-down deselections across runs
    /// (CONTEXT.md's "Drill-down": "Deselections persist in the Profile").
    pub fn deselected_paths(&self) -> &BTreeSet<PathBuf> {
        &self.deselected
    }

    /// Every Entry belonging to `category` — active or withheld — for a Drill-down UI to
    /// list with per-Entry checkbox state (`is_deselected`), independently of whether the
    /// Category itself is checked.
    pub fn entries_in_category(&self, category: &str) -> Vec<&Entry> {
        self.entries
            .iter()
            .chain(self.withheld.iter())
            .filter(|e| e.category == category)
            .collect()
    }

    /// The absolute, on-disk paths of every selected Entry.
    pub fn selected_paths(&self) -> Vec<&Path> {
        self.entries
            .iter()
            .map(|e| e.absolute_path.as_path())
            .collect()
    }

    /// The sum of every selected Entry's current on-disk size in bytes.
    ///
    /// Recomputed from disk each call rather than cached, so it reflects whatever is
    /// actually there at read time (a file growing/shrinking between scan and backup is
    /// not this method's concern).
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|e| std::fs::metadata(&e.absolute_path).ok())
            .map(|m| m.len())
            .sum()
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
            .iter()
            .filter(|e| e.category == catalog::USER_DATA.name)
            .filter_map(|e| std::fs::metadata(&e.absolute_path).ok())
            .map(|m| m.len())
            .sum();

        let mut warnings = Vec::new();
        if user_data_size > USER_DATA_WARNING_THRESHOLD_BYTES {
            warnings.push(format!(
                "User data selection is {:.2} GB — this Category is large and default-OFF for a reason",
                user_data_size as f64 / USER_DATA_WARNING_THRESHOLD_BYTES as f64
            ));
        }
        warnings
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
        // (a `BTreeSet` snapshot for O(log n) membership) down with a generous, non-flaky
        // time budget: comfortably above what the fixed version needs (single-digit
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
}
