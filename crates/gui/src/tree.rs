//! The lazy `["tree", prefix]` query (ADR-0008 Decision 2 "Views out", issue #100): a
//! prefix's immediate children plus each child's `directory_state`. No eager whole-tree
//! snapshot ever crosses IPC — a frontend node only ever needs its own row plus enough to
//! know whether an expand affordance belongs on it.
//!
//! Two levels: the root level lists Categories (CONTEXT.md's "Category", Hide-empty
//! applied); every other level lists the directory/file children of a Category's own
//! Entries under a `$HOME`-relative prefix. This mirrors `crates/tui/src/what_screen.rs`'s
//! `explorer_rows`/`ChildAccumulator` grouping (ADR-0006 D5: the navigable tree is
//! presentation grouping a UI derives, not a `core` change) independently rather than
//! sharing code with it (ADR-0001/ADR-0008: no `crates/ui-common` until real duplication
//! demands one) — this version is deliberately leaner (no per-row child counts or on-disk
//! sizes), since issue #100's own scope needs only tri-state and leaf-ness.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::Path;

use linux_backup_core::catalog;
use linux_backup_core::model::{Entry, EntryType};
use linux_backup_core::scan::CategoryScan;
use linux_backup_core::{DirectoryState, SelectionModel};

use crate::dto::DirectoryStateDto;

/// What a [`TreeNodeDto`] represents: a whole Category (the tree's root level) or a
/// directory/file node drilled into from one (ADR-0006's recursive Drill-down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeNodeKindDto {
    Category,
    Directory,
    File,
}

/// One row of the lazy `["tree", prefix]` query.
///
/// `camelCase` on the wire, same rationale as [`crate::dto::SelectionSummaryDto`]: this is
/// consumed by the `TanStack` Query frontend over IPC, not a persisted file format.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNodeDto {
    /// A stable, globally-unique key for the frontend's tree component. Never parsed back by
    /// any command — the mutation commands take `category`/`relative_path` directly, not
    /// this id.
    pub id: String,
    /// The rendered label: the Category name, or the node's own path component.
    pub name: String,
    /// The Category this node belongs to (its own name, for a Category node).
    pub category: String,
    /// The `$HOME`-relative path the mutation commands (`deselect_entry`/`deselect_root`/
    /// their reselect counterparts) take. `None` only for a Category (root) node, whose
    /// mutation is `select_category`/`deselect_category` instead.
    pub relative_path: Option<String>,
    pub kind: TreeNodeKindDto,
    pub state: DirectoryStateDto,
    /// Whether this node has anything beneath it worth a further `["tree", ...]` query — a
    /// leaf (a File, an empty directory, or an empty Category) never issues one.
    pub has_children: bool,
    /// True only for the empty Custom-paths Category node (CONTEXT.md story 6's "always
    /// shown" add affordance) — the frontend's cue to render the add-a-path control.
    pub add_affordance: bool,
}

/// The tree's root level (issue #100): one node per Category that survives Hide-empty
/// (CONTEXT.md: "a discoverable Category with no matching paths on this machine is
/// hidden"), plus Custom paths unconditionally (`Category::always_shown`).
///
/// `scanned` must be a fresh `scan::scan_taxonomy` result — the on-disk truth Hide-empty
/// needs. `selection.entries_in_category` alone cannot answer Hide-empty for a Category the
/// Selection has never touched (Smart default OFF and never toggled on): such a Category has
/// no Entries of its own in `selection` yet at all, known or withheld, even though real files
/// may exist on disk waiting to be toggled on.
pub fn category_nodes(selection: &SelectionModel, scanned: &[CategoryScan]) -> Vec<TreeNodeDto> {
    let active_paths: HashSet<&Path> = selection
        .entries()
        .iter()
        .map(|entry| entry.relative_path.as_path())
        .collect();

    scanned
        .iter()
        .map(|scan| category_node(selection, scan, &active_paths))
        .collect()
}

/// Builds one Category's root [`TreeNodeDto`]. Custom paths is special-cased: its own
/// allowlist is always empty (`scan_taxonomy` never discovers it, per
/// `catalog::CUSTOM_PATHS`'s doc), so `scan.entries` is always empty for it even when the
/// Selection already holds interactively-added Custom paths — its tri-state instead reads
/// the model directly (`entries_in_category`), exactly like every mutation it ever receives.
fn category_node(
    selection: &SelectionModel,
    scan: &CategoryScan,
    active_paths: &HashSet<&Path>,
) -> TreeNodeDto {
    let is_custom_paths = scan.category.name == catalog::CUSTOM_PATHS.name;
    let (total, active) = if is_custom_paths {
        let known = selection.entries_in_category(scan.category.name);
        let active = known
            .iter()
            .filter(|entry| active_paths.contains(entry.relative_path.as_path()))
            .count();
        (known.len(), active)
    } else {
        let active = scan
            .entries
            .iter()
            .filter(|entry| active_paths.contains(entry.relative_path.as_path()))
            .count();
        (scan.entries.len(), active)
    };

    let state = if active == 0 {
        DirectoryState::None
    } else if active == total {
        DirectoryState::All
    } else {
        DirectoryState::Partial
    };

    TreeNodeDto {
        id: format!("category:{}", scan.category.name),
        name: scan.category.name.to_string(),
        category: scan.category.name.to_string(),
        relative_path: None,
        kind: TreeNodeKindDto::Category,
        state: state.into(),
        has_children: total > 0,
        add_affordance: is_custom_paths && total == 0,
    }
}

/// One immediate child under `prefix`, scoped to `category` only (ADR-0006 D5's
/// per-Category explorer) — the lazy per-directory descent issue #100 asks for. `prefix` is
/// the `$HOME`-relative path already drilled into; an empty [`Path`] means the Category's
/// own top level.
///
/// Sourced from `active_entries_under`/`withheld_entries_under` (#83's range-scan
/// machinery), the same way `crates/tui`'s `explorer_rows` is: bounded to the `k` Entries
/// actually under `prefix`, not a scan of the whole Selection. A node's tri-state accrues
/// across *every* Category sharing the path (matching `directory_state`'s own Category-
/// agnostic rule — ADR-0006's prefix-wins exclusion has no Category boundary), while a
/// node's existence and kind stay scoped to the focused `category`.
pub fn category_children(
    selection: &SelectionModel,
    category: &str,
    prefix: &Path,
) -> Vec<TreeNodeDto> {
    let mut groups: BTreeMap<OsString, ChildAccumulator> = BTreeMap::new();

    for entry in selection.active_entries_under(prefix) {
        accumulate(&mut groups, prefix, entry, category, true);
    }
    for entry in selection.withheld_entries_under(prefix) {
        accumulate(&mut groups, prefix, entry, category, false);
    }

    let mut rows: Vec<TreeNodeDto> = groups
        .into_iter()
        .filter(|(_, accumulator)| accumulator.in_category)
        .map(|(component, accumulator)| build_node(category, prefix, &component, &accumulator))
        .collect();

    // Directories first, then files, alphabetical case-insensitive within each group — the
    // same scannable ordering `crates/tui`'s `explorer_rows` renders in.
    rows.sort_by(|a, b| {
        is_file(a.kind)
            .cmp(&is_file(b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

fn is_file(kind: TreeNodeKindDto) -> bool {
    matches!(kind, TreeNodeKindDto::File)
}

fn build_node(
    category: &str,
    prefix: &Path,
    component: &OsString,
    accumulator: &ChildAccumulator,
) -> TreeNodeDto {
    let node_path = prefix.join(component);
    let is_directory = accumulator.has_grandchildren || accumulator.exact_is_directory;
    let name = component.to_string_lossy().into_owned();
    TreeNodeDto {
        id: format!("category:{category}:{}", node_path.display()),
        name,
        category: category.to_string(),
        relative_path: Some(node_path.to_string_lossy().into_owned()),
        kind: if is_directory {
            TreeNodeKindDto::Directory
        } else {
            TreeNodeKindDto::File
        },
        state: accumulator.state().into(),
        has_children: accumulator.has_grandchildren,
        add_affordance: false,
    }
}

/// Accumulates, per immediate child component under the current prefix, whether it has
/// deeper Entries (a real directory) and the tri-state flags a row's [`DirectoryState`]
/// falls out of — the same shape as `crates/tui`'s `ChildAccumulator`, minus the
/// presentation-only child-count/size fields this DTO doesn't carry.
#[derive(Default)]
struct ChildAccumulator {
    in_category: bool,
    any_active: bool,
    any_withheld: bool,
    has_grandchildren: bool,
    exact_is_directory: bool,
}

impl ChildAccumulator {
    fn state(&self) -> DirectoryState {
        match (self.any_active, self.any_withheld) {
            (true, false) => DirectoryState::All,
            (true, true) => DirectoryState::Partial,
            (false, _) => DirectoryState::None,
        }
    }
}

/// Folds one Entry, known to be under `prefix`, into its row's [`ChildAccumulator`] — shared
/// by both the active and withheld passes, `active` recording which one this Entry came
/// from. The `any_active`/`any_withheld` tri-state flags accrue from every Category (so a
/// row's state matches Category-agnostic `directory_state`); a row's existence and kind
/// accrue only from `category`.
fn accumulate(
    groups: &mut BTreeMap<OsString, ChildAccumulator>,
    prefix: &Path,
    entry: &Entry,
    category: &str,
    active: bool,
) {
    let Ok(rest) = entry.relative_path.strip_prefix(prefix) else {
        return;
    };
    let mut components = rest.components();
    let Some(first) = components.next() else {
        return; // the node itself, not one of its children
    };
    let accumulator = groups.entry(first.as_os_str().to_os_string()).or_default();
    if active {
        accumulator.any_active = true;
    } else {
        accumulator.any_withheld = true;
    }
    if entry.category != category {
        return;
    }
    accumulator.in_category = true;
    if components.next().is_some() {
        accumulator.has_grandchildren = true;
    } else if entry.entry_type == EntryType::Directory {
        accumulator.exact_is_directory = true;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use linux_backup_core::catalog;
    use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
    use linux_backup_core::scan::CategoryScan;

    use super::{category_children, category_nodes, TreeNodeKindDto};

    fn entry(category: &'static str, relative_path: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/nonexistent/{relative_path}")),
            relative_path: PathBuf::from(relative_path),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive: false,
        }
    }

    #[test]
    fn category_nodes_hides_a_category_with_zero_scanned_entries() {
        let selection = SelectionModel::new(vec![]);
        let scanned = vec![CategoryScan {
            category: catalog::CUSTOM_PATHS,
            entries: vec![],
        }];
        // Hide-empty exempts only `always_shown` Categories (Custom paths); a non-exempt
        // empty Category would already have been dropped by `scan_taxonomy` itself before
        // reaching here, so this only pins Custom paths' own "always shown" carve-out.
        let nodes = category_nodes(&selection, &scanned);
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].add_affordance);
        assert!(!nodes[0].has_children);
    }

    #[test]
    fn category_children_groups_a_directory_with_mixed_state_as_partial() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, "kwinrc"),
            entry(catalog::KDE_PLASMA.name, "icons/a.svg"),
            entry(catalog::KDE_PLASMA.name, "icons/b.svg"),
        ]);
        selection.deselect_entry(std::path::Path::new("icons/b.svg"));

        let rows = category_children(
            &selection,
            catalog::KDE_PLASMA.name,
            std::path::Path::new(""),
        );

        let kwinrc = rows.iter().find(|row| row.name == "kwinrc").unwrap();
        assert_eq!(kwinrc.kind, TreeNodeKindDto::File);
        assert_eq!(kwinrc.state, super::DirectoryStateDto::All);
        assert!(!kwinrc.has_children);

        let icons = rows.iter().find(|row| row.name == "icons").unwrap();
        assert_eq!(icons.kind, TreeNodeKindDto::Directory);
        assert_eq!(icons.state, super::DirectoryStateDto::Partial);
        assert!(icons.has_children);
    }

    #[test]
    fn category_children_of_a_fully_selected_directory_reports_all_and_none_when_fully_withheld() {
        let mut selection = SelectionModel::new(vec![
            entry(catalog::KDE_PLASMA.name, "icons/a.svg"),
            entry(catalog::KDE_PLASMA.name, "icons/b.svg"),
        ]);
        selection.deselect_root(std::path::Path::new("icons/b.svg"));

        let rows = category_children(
            &selection,
            catalog::KDE_PLASMA.name,
            std::path::Path::new("icons"),
        );

        let a = rows.iter().find(|row| row.name == "a.svg").unwrap();
        assert_eq!(a.state, super::DirectoryStateDto::All);
        let b = rows.iter().find(|row| row.name == "b.svg").unwrap();
        assert_eq!(b.state, super::DirectoryStateDto::None);
    }

    #[test]
    fn category_children_is_empty_under_an_unknown_prefix() {
        let selection = SelectionModel::new(vec![entry(catalog::KDE_PLASMA.name, "kwinrc")]);

        let rows = category_children(
            &selection,
            catalog::KDE_PLASMA.name,
            std::path::Path::new("nonexistent"),
        );

        assert!(rows.is_empty());
    }
}
