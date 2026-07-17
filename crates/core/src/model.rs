//! The `SelectionModel`: Category defaults, precedence resolution, Drill-down toggles, and
//! derived state (`encryption_required`, `total_size`, `selected_paths`, `warnings`).
//!
//! This is `core`'s authoritative model of what will be backed up; `crates/tui` is a thin
//! view over it, owning only cursor/scroll state and rendering.
//!
//! #20 scope: `SelectionModel::from_smart_defaults` composes a Smart-default Selection
//! across the full ten-Category taxonomy (`scan::scan_taxonomy`'s precedence + Hide-empty
//! result), and `warnings()` derives the User-data size warning (story 4) over whatever
//! Categories ended up in the Selection. Drill-down toggling is #21's job.
//!
//! #22 scope: `encryption_required` delegates to `crypto::evaluate`'s Auto-insist policy —
//! see that module for which specific Categories/Entries triggered it.

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
    /// Whether this Entry is Sensitive: true if its Category is Sensitive, OR the
    /// Safety-net scan flagged its filename (`scan::is_safety_net_match`) — a strict
    /// superset, per CONTEXT.md's "Sensitive" entry. Set once at scan time; never
    /// re-derived here.
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
    entries: Vec<Entry>,
}

impl SelectionModel {
    /// Builds a Selection from an already-scanned, already-selected set of Entries. Useful
    /// on its own for a single Category (as the round-trip seam's `shell_selection` helper
    /// does) or once Drill-down (#21) has finished toggling individual Entries within an
    /// otherwise Smart-defaulted Selection.
    pub fn new(entries: Vec<Entry>) -> Self {
        Self { entries }
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
        Self { entries }
    }

    /// The selected Entries, in scan order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
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
    /// 15/17: any Sensitive Entry — Category-level or Safety-net — forces it on). A thin
    /// delegate to `crypto::evaluate`, which also exposes exactly which Categories/Entries
    /// triggered it (story 18) for a TUI to show before prompting for a passphrase.
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
}
