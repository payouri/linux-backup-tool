//! The Rust boundary-translation seam (spec #97's Testing Decisions, issue #99): drives the
//! pure translation layer and asserts the **serialized JSON wire payloads** — the
//! externally-observable contract the frontend depends on — never internal struct fields.
//! Prior art: `crates/core/tests/roundtrip.rs`.

// This crate is only ever built as a test binary (`--test`), so this allow is unconditional
// — AGENTS.md exempts test code from the no-unwrap/expect/panic rule (mirrors
// crates/core/tests/roundtrip.rs).
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::fs;
use std::path::{Path, PathBuf};

use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
use linux_backup_core::CoreError;
use linux_backup_gui::{SelectionSummaryDto, SerializableCoreError};

fn entry_at(
    root: &Path,
    category: &'static str,
    sensitive: bool,
    name: &str,
    bytes: &[u8],
) -> Entry {
    let absolute_path = root.join(name);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).expect("create entry parent dir");
    }
    fs::write(&absolute_path, bytes).expect("write entry file");
    Entry {
        absolute_path,
        relative_path: PathBuf::from(name),
        root: EntryRoot::Home,
        entry_type: EntryType::File,
        category,
        sensitive,
    }
}

#[test]
fn a_fully_selected_entry_serializes_the_expected_summary_shape() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let entry = entry_at(
        tempdir.path(),
        "SSH",
        true,
        "id_ed25519",
        b"secret-key-bytes",
    );
    let selection = SelectionModel::new(vec![entry]);

    let dto = SelectionSummaryDto::from(&selection);
    let json = serde_json::to_value(&dto).expect("serialize SelectionSummaryDto");

    assert_eq!(
        json,
        serde_json::json!({
            "entryCount": 1,
            "totalSizeBytes": 16,
            "isEmpty": false,
            "encryptionRequired": true,
            "warnings": [],
        })
    );
}

#[test]
fn an_empty_selection_serializes_is_empty_true_and_no_encryption_requirement() {
    let selection = SelectionModel::new(vec![]);

    let dto = SelectionSummaryDto::from(&selection);
    let json = serde_json::to_value(&dto).expect("serialize SelectionSummaryDto");

    assert_eq!(
        json,
        serde_json::json!({
            "entryCount": 0,
            "totalSizeBytes": 0,
            "isEmpty": true,
            "encryptionRequired": false,
            "warnings": [],
        })
    );
}

#[test]
fn a_non_sensitive_selection_does_not_require_encryption() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let contents: &[u8] = b"export PATH=$PATH";
    let entry = entry_at(tempdir.path(), "Shell", false, ".zshrc", contents);
    let selection = SelectionModel::new(vec![entry]);

    let dto = SelectionSummaryDto::from(&selection);

    assert!(!dto.encryption_required);
    assert_eq!(dto.total_size_bytes, contents.len() as u64);
}

#[test]
fn blank_passphrase_serializes_to_its_kind_and_message() {
    let error = SerializableCoreError::from(&CoreError::BlankPassphrase);
    let json = serde_json::to_value(&error).expect("serialize SerializableCoreError");

    assert_eq!(
        json,
        serde_json::json!({
            "kind": "blank_passphrase",
            "message": "passphrase must not be blank",
        })
    );
}

#[test]
fn a_path_bearing_core_error_flattens_its_pathbuf_into_the_message_string() {
    let error =
        SerializableCoreError::from(&CoreError::BackupDirExists(PathBuf::from("/home/tester/x")));
    let json = serde_json::to_value(&error).expect("serialize SerializableCoreError");

    assert_eq!(
        json,
        serde_json::json!({
            "kind": "backup_dir_exists",
            "message": "backup dir already exists: /home/tester/x",
        })
    );
}
