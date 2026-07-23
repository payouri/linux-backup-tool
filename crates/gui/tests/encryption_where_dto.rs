//! The Rust boundary-translation seam (spec #97's Testing Decisions, issue #101): drives the
//! encryption-status and WHERE pre-check translation layers and asserts the **serialized JSON
//! wire payloads** — the externally-observable contract the frontend depends on. Prior art:
//! `crates/gui/tests/selection_summary_dto.rs`, `crates/gui/tests/tree_dto.rs`.

// This crate is only ever built as a test binary (`--test`), so this allow is unconditional
// — AGENTS.md exempts test code from the no-unwrap/expect/panic rule (mirrors
// crates/core/tests/roundtrip.rs).
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::path::{Path, PathBuf};

use linux_backup_core::location::{self, LocationSelection};
use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
use linux_backup_core::CoreError;
use linux_backup_gui::{
    encryption_status, EncryptionDecision, SerializableCoreError, WhereCheckDto,
};

fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
    Entry {
        absolute_path: PathBuf::from(format!("/home/tester/{name}")),
        relative_path: PathBuf::from(name),
        root: EntryRoot::Home,
        entry_type: EntryType::File,
        category,
        sensitive,
    }
}

#[test]
fn a_sensitive_selection_serializes_encryption_status_as_required_and_incomplete() {
    let selection = SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]);
    let decision = EncryptionDecision::new(&selection);

    let dto = encryption_status(&selection, &decision);
    let json = serde_json::to_value(&dto).expect("serialize EncryptionStatusDto");

    assert_eq!(
        json,
        serde_json::json!({
            "autoInsistRequired": true,
            "sensitiveCategories": ["SSH"],
            "hasPassphrase": false,
            "isComplete": false,
            "isInvalidated": false,
        })
    );
}

#[test]
fn a_non_sensitive_selection_serializes_encryption_status_as_complete_on_arrival() {
    let selection = SelectionModel::new(vec![entry("Shell", false, ".zshrc")]);
    let decision = EncryptionDecision::new(&selection);

    let dto = encryption_status(&selection, &decision);
    let json = serde_json::to_value(&dto).expect("serialize EncryptionStatusDto");

    assert_eq!(
        json,
        serde_json::json!({
            "autoInsistRequired": false,
            "sensitiveCategories": [],
            "hasPassphrase": false,
            "isComplete": true,
            "isInvalidated": false,
        })
    );
}

#[test]
fn adding_sensitive_content_serializes_a_completed_plaintext_decision_as_invalidated() {
    let mut selection = SelectionModel::new(vec![entry("Shell", false, ".zshrc")]);
    let decision = EncryptionDecision::new(&selection);
    assert!(encryption_status(&selection, &decision).is_complete);

    // A WHAT edit introduces Sensitive content the plaintext decision didn't cover (ADR-0007
    // D5): the frontier collapses back to encryption and the step reads as invalidated.
    selection.add_custom_path(
        Path::new("/home/tester/.ssh/id_ed25519"),
        Path::new("/home/tester"),
    );

    let dto = encryption_status(&selection, &decision);
    let json = serde_json::to_value(&dto).expect("serialize EncryptionStatusDto");

    assert_eq!(
        json,
        serde_json::json!({
            "autoInsistRequired": true,
            "sensitiveCategories": ["Custom paths"],
            "hasPassphrase": false,
            "isComplete": false,
            "isInvalidated": true,
        })
    );
}

#[test]
fn blank_passphrase_still_flattens_to_its_kind_and_message_for_the_encryption_step() {
    // Story 24: the encryption step's blank-passphrase rejection reuses the existing
    // `CoreError::BlankPassphrase` mapping rather than inventing a new wire shape.
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
fn where_check_dto_serializes_the_resolved_location_and_backup_dir() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let resolved: LocationSelection =
        location::resolve_selection(tempdir.path(), "linux-backup-host-1", 0, None)
            .expect("a fresh tempdir resolves cleanly");

    let dto = WhereCheckDto::from(&resolved);
    let json = serde_json::to_value(&dto).expect("serialize WhereCheckDto");

    assert_eq!(
        json,
        serde_json::json!({
            "location": tempdir.path().to_string_lossy(),
            "backupDir": tempdir.path().join("linux-backup-host-1").to_string_lossy(),
        })
    );
}

#[test]
fn where_check_surfaces_a_backup_dir_collision_as_a_typed_error() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    std::fs::create_dir(tempdir.path().join("linux-backup-host-1"))
        .expect("create the colliding dir");

    let err = location::resolve_selection(tempdir.path(), "linux-backup-host-1", 0, None)
        .expect_err("an existing Backup dir name must be refused");
    let serializable = SerializableCoreError::from(&err);

    assert_eq!(serializable.kind, "backup_dir_exists");
}
