//! The Rust boundary-translation seam (spec #97's Testing Decisions, issue #100): drives the
//! lazy `["tree", prefix]` translation layer and asserts the **serialized JSON wire
//! payloads** — the externally-observable contract the frontend depends on — never internal
//! struct fields. Prior art: `crates/gui/tests/selection_summary_dto.rs`.

// This crate is only ever built as a test binary (`--test`), so this allow is unconditional
// — AGENTS.md exempts test code from the no-unwrap/expect/panic rule (mirrors
// crates/core/tests/roundtrip.rs).
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::path::{Path, PathBuf};

use linux_backup_core::catalog;
use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
use linux_backup_core::scan::CategoryScan;
use linux_backup_gui::DirectoryStateDto;

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
fn a_fully_selected_subtree_serializes_the_directory_state_as_all() {
    let selection = SelectionModel::new(vec![entry(catalog::KDE_PLASMA.name, "icons/a.svg")]);

    let dto = DirectoryStateDto::from(selection.directory_state(Path::new("icons")));
    let json = serde_json::to_value(dto).expect("serialize DirectoryStateDto");

    assert_eq!(json, serde_json::json!("all"));
}

#[test]
fn a_mixed_subtree_serializes_the_directory_state_as_partial() {
    let mut selection = SelectionModel::new(vec![
        entry(catalog::KDE_PLASMA.name, "icons/a.svg"),
        entry(catalog::KDE_PLASMA.name, "icons/b.svg"),
    ]);
    selection.deselect_entry(Path::new("icons/b.svg"));

    let dto = DirectoryStateDto::from(selection.directory_state(Path::new("icons")));
    let json = serde_json::to_value(dto).expect("serialize DirectoryStateDto");

    assert_eq!(json, serde_json::json!("partial"));
}

#[test]
fn an_unknown_prefix_serializes_the_directory_state_as_none() {
    let selection = SelectionModel::new(vec![entry(catalog::KDE_PLASMA.name, "kwinrc")]);

    let dto = DirectoryStateDto::from(selection.directory_state(Path::new("nonexistent")));
    let json = serde_json::to_value(dto).expect("serialize DirectoryStateDto");

    assert_eq!(json, serde_json::json!("none"));
}

#[test]
fn category_root_nodes_serialize_the_expected_wire_shape() {
    let selection =
        SelectionModel::new(vec![entry(catalog::SHELL.name, ".oh-my-zsh/custom/a.zsh")]);
    let scanned = vec![CategoryScan {
        category: catalog::SHELL,
        entries: vec![entry(catalog::SHELL.name, ".oh-my-zsh/custom/a.zsh")],
    }];

    let nodes = linux_backup_gui::category_nodes(&selection, &scanned);
    let json = serde_json::to_value(&nodes).expect("serialize tree node DTOs");

    assert_eq!(
        json,
        serde_json::json!([{
            "id": "category:Shell",
            "name": "Shell",
            "category": "Shell",
            "relativePath": null,
            "kind": "category",
            "state": "all",
            "hasChildren": true,
            "addAffordance": false,
        }])
    );
}

#[test]
fn custom_paths_is_shown_empty_with_the_add_affordance_even_with_no_scanned_entries() {
    let selection = SelectionModel::new(vec![]);
    let scanned = vec![CategoryScan {
        category: catalog::CUSTOM_PATHS,
        entries: vec![],
    }];

    let nodes = linux_backup_gui::category_nodes(&selection, &scanned);
    let json = serde_json::to_value(&nodes).expect("serialize tree node DTOs");

    assert_eq!(
        json,
        serde_json::json!([{
            "id": "category:Custom paths",
            "name": "Custom paths",
            "category": "Custom paths",
            "relativePath": null,
            "kind": "category",
            "state": "none",
            "hasChildren": false,
            "addAffordance": true,
        }])
    );
}

#[test]
fn category_children_serialize_a_directory_node_and_a_file_leaf() {
    let selection = SelectionModel::new(vec![
        entry(catalog::KDE_PLASMA.name, "kwinrc"),
        entry(catalog::KDE_PLASMA.name, "icons/a.svg"),
    ]);

    let rows =
        linux_backup_gui::category_children(&selection, catalog::KDE_PLASMA.name, Path::new(""));
    let json = serde_json::to_value(&rows).expect("serialize tree node DTOs");

    assert_eq!(
        json,
        serde_json::json!([
            {
                "id": "category:KDE Plasma:icons",
                "name": "icons",
                "category": "KDE Plasma",
                "relativePath": "icons",
                "kind": "directory",
                "state": "all",
                "hasChildren": true,
                "addAffordance": false,
            },
            {
                "id": "category:KDE Plasma:kwinrc",
                "name": "kwinrc",
                "category": "KDE Plasma",
                "relativePath": "kwinrc",
                "kind": "file",
                "state": "all",
                "hasChildren": false,
                "addAffordance": false,
            },
        ])
    );
}
