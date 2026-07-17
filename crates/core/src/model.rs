//! The `SelectionModel`: Category defaults, precedence resolution, Drill-down toggles, and
//! derived state (`encryption_required`, `total_size`, `selected_paths`, `warnings`).
//!
//! This is `core`'s authoritative model of what will be backed up; `crates/tui` is a thin
//! view over it, owning only cursor/scroll state and rendering.
//!
//! #19 scope: just enough to represent "these Entries are selected" and derive
//! `selected_paths`/`total_size` for the round-trip seam. Category defaults, Drill-down
//! toggles, `encryption_required`, and `warnings` are later sub-issues' jobs (#20-#22).

use std::path::{Path, PathBuf};

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
    /// Whether this Entry is Sensitive (inherited from its Category for #19; per-entry
    /// tagging and the Safety-net scan are #22's job).
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
    /// Builds a Selection from an already-scanned, already-selected set of Entries.
    /// Smart defaults / Drill-down toggling over the full Category taxonomy are #20-#21's
    /// job; #19 only needs to represent the result of that process.
    pub fn new(entries: Vec<Entry>) -> Self {
        Self { entries }
    }

    /// The selected Entries, in scan order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The absolute, on-disk paths of every selected Entry.
    pub fn selected_paths(&self) -> Vec<&Path> {
        self.entries.iter().map(|e| e.absolute_path.as_path()).collect()
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
}
