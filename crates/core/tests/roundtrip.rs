//! The one round-trip seam through `core`'s public API (issue #19, per the parent spec
//! #17's "Testing Decisions"): build a synthetic `$HOME` Fixture environment, run a real
//! backup into a temp Output location, then restore that Backup dir into a *different*
//! fresh temp root, asserting only externally-observable behavior — which Entries landed,
//! the Manifest's contents, byte-for-byte restored file contents, and Re-rooting.
//!
//! This file is meant to be **extended**, not replaced, by every later sub-issue that
//! grows a new Category, sensitivity rule, failure mode, or restore mechanic. The helpers
//! below (`build_fixture_home`, `test_ctx`, `shell_selection`, `run_backup`, `run_restore`)
//! exist so a new test case can be a few lines of setup + assertions rather than
//! re-deriving the whole seam.
//!
//! Per #19's scope, the only Category exercised here is Shell (`catalog::SHELL`,
//! `.oh-my-zsh/custom/` only) — enough to drive the seam end-to-end without needing the
//! full ten-Category taxonomy (#20) or sensitivity/encryption policy (#22/#23).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use linux_backup_core::archive::Encryption;
use linux_backup_core::context::{Privilege, RunContext};
use linux_backup_core::model::SelectionModel;
use linux_backup_core::restore::RestoreOutcome;
use linux_backup_core::{catalog, manifest, scan, BackupOutcome, CoreError};

/// Builds a synthetic `$HOME` under `root` containing:
/// - two files *on* the Shell Category's Curated allowlist (`.oh-my-zsh/custom/...`),
/// - one file *outside* it (`.oh-my-zsh/themes/...`) that must never be captured, proving
///   the allowlist is curated rather than a whole-directory copy (CONTEXT.md's "Curated
///   allowlist"),
/// - one file entirely outside `oh-my-zsh` that must also never be captured.
///
/// Returns the Fixture environment's home path.
fn build_fixture_home(root: &Path) -> PathBuf {
    let home = root.join("home");

    let custom_dir = home.join(".oh-my-zsh/custom");
    fs::create_dir_all(&custom_dir).expect("create custom dir");
    fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n").expect("write aliases.zsh");

    let plugin_dir = custom_dir.join("plugins/my-plugin");
    fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    fs::write(plugin_dir.join("my-plugin.plugin.zsh"), b"# my plugin\necho hi\n").expect("write plugin file");

    // Outside the Curated allowlist (Shell only covers oh-my-zsh/custom/, never the whole
    // framework) — must not be picked up by scan_category.
    let themes_dir = home.join(".oh-my-zsh/themes");
    fs::create_dir_all(&themes_dir).expect("create themes dir");
    fs::write(themes_dir.join("robbyrussell.zsh-theme"), b"# theme\n").expect("write theme file");

    // Outside oh-my-zsh entirely.
    fs::write(home.join("not-included.txt"), b"irrelevant\n").expect("write unrelated file");

    home
}

/// A fixed, fully-injected `RunContext` (ADR-0004: nothing ambient) for a fixture rooted
/// at `home`, backing up into (or restoring relative to) `output_location`.
fn test_ctx(home: PathBuf, output_location: PathBuf) -> RunContext {
    RunContext {
        home,
        output_location,
        privilege: Privilege::NonRoot,
        host: "test-host".to_string(),
        user: "test-user".to_string(),
        uid: 1000,
        os: "linux-test".to_string(),
        now: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    }
}

/// A `SelectionModel` scoped to the Shell Category's Curated allowlist under `ctx.home`.
fn shell_selection(ctx: &RunContext) -> SelectionModel {
    let entries = scan::scan_category(&catalog::SHELL, ctx);
    SelectionModel::new(entries)
}

fn run_backup(selection: &SelectionModel, ctx: &RunContext, encryption: &Encryption) -> Result<BackupOutcome, CoreError> {
    linux_backup_core::backup(selection, ctx, encryption)
}

fn run_restore(backup_dir: &Path, ctx: &RunContext) -> Result<RestoreOutcome, CoreError> {
    linux_backup_core::restore::restore(backup_dir, ctx)
}

/// Asserts a completed backup+restore round-trip landed correctly: no restore failures,
/// both fixture files restored byte-for-byte under `restore_home` (not the original
/// `original_home` — Re-rooting), and the out-of-allowlist file never appearing anywhere
/// in the Backup.
fn assert_round_trip_landed(outcome: &BackupOutcome, restore_outcome: &RestoreOutcome, original_home: &Path, restore_home: &Path) {
    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(restore_outcome.restored_paths.len(), 2);
    assert_eq!(outcome.manifest.entries.len(), 2);
    assert_eq!(outcome.manifest.checksums.len(), 2);
    assert!(outcome.manifest.vanished.is_empty());

    let original_aliases = original_home.join(".oh-my-zsh/custom/aliases.zsh");
    let original_plugin = original_home.join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh");
    let restored_aliases = restore_home.join(".oh-my-zsh/custom/aliases.zsh");
    let restored_plugin = restore_home.join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh");

    assert_eq!(
        fs::read(&restored_aliases).expect("restored aliases.zsh should exist"),
        fs::read(&original_aliases).expect("original aliases.zsh should exist"),
        "restored aliases.zsh must be byte-for-byte identical to the original"
    );
    assert_eq!(
        fs::read(&restored_plugin).expect("restored plugin file should exist"),
        fs::read(&original_plugin).expect("original plugin file should exist"),
        "restored plugin file must be byte-for-byte identical to the original"
    );

    // Re-rooting: home-tagged Entries land under the *restoring* context's home, not the
    // original one.
    assert_ne!(restore_home, original_home);
    assert!(restore_outcome
        .restored_paths
        .iter()
        .all(|p| p.starts_with(restore_home)));

    // The out-of-allowlist theme file was never captured, so restore has nothing to land.
    assert!(!restore_home.join(".oh-my-zsh/themes").exists());
}

#[test]
fn round_trip_unencrypted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let selection = shell_selection(&ctx);

    // Only the Curated allowlist's two files land in the Selection, never the whole
    // oh-my-zsh tree (CONTEXT.md's "Curated allowlist").
    assert_eq!(selection.entries().len(), 2);
    assert!(selection.total_size() > 0);

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let expected_dir_name = format!(
        "linux-backup-{}-{}",
        ctx.host,
        ctx.now.duration_since(UNIX_EPOCH).unwrap().as_secs()
    );
    assert_eq!(
        outcome.backup_dir.file_name().and_then(|n| n.to_str()),
        Some(expected_dir_name.as_str())
    );

    let archive_path = outcome.backup_dir.join("archive.tar.zst");
    let manifest_path = outcome.backup_dir.join("manifest.json");
    assert!(archive_path.is_file(), "unencrypted Archive should have no .age suffix");
    assert!(manifest_path.is_file(), "unencrypted Manifest should have no .age suffix");
    assert!(!outcome.backup_dir.join("archive.tar.zst.age").exists());
    assert!(!outcome.backup_dir.join("manifest.json.age").exists());

    let written_manifest: manifest::Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).expect("manifest.json should parse");
    assert_eq!(written_manifest.schema_version, outcome.manifest.schema_version);

    let m = &outcome.manifest;
    assert_eq!(m.schema_version, manifest::SCHEMA_VERSION);
    assert_eq!(m.host, ctx.host);
    assert_eq!(m.user, ctx.user);
    assert_eq!(m.uid, ctx.uid);
    assert_eq!(m.os, ctx.os);
    assert_eq!(m.home, ctx.home);
    assert_eq!(m.created_at, "2023-11-14T22:13:20Z");
    assert!(m.packages.is_empty());

    let archive_bytes_on_disk = fs::read(&archive_path).unwrap();
    // Unencrypted: the archive_sha256 in the Manifest is of the plaintext bytes, which
    // (with Encryption::None) are exactly what's on disk.
    assert_eq!(m.archive_sha256, linux_backup_core::archive::sha256_hex(&archive_bytes_on_disk));

    // Restore into a DIFFERENT fresh root.
    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output.clone());
    let restore_outcome = run_restore(&outcome.backup_dir, &restore_ctx).expect("restore should succeed");

    assert_round_trip_landed(&outcome, &restore_outcome, &home, &restore_home);
}

#[test]
fn round_trip_encrypted_stub() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let selection = shell_selection(&ctx);
    let encryption = Encryption::Passphrase("correct-horse-battery-staple".to_string());

    let outcome = run_backup(&selection, &ctx, &encryption).expect("backup should succeed");

    // Encrypted path: both the Archive and Manifest carry the .age suffix (CONTEXT.md:
    // encryption mirrors across both, all-or-nothing), and the unencrypted filenames must
    // not exist.
    let archive_path = outcome.backup_dir.join("archive.tar.zst.age");
    let manifest_path = outcome.backup_dir.join("manifest.json.age");
    assert!(archive_path.is_file(), "encrypted Archive should carry the .age suffix");
    assert!(manifest_path.is_file(), "encrypted Manifest should carry the .age suffix");
    assert!(!outcome.backup_dir.join("archive.tar.zst").exists());
    assert!(!outcome.backup_dir.join("manifest.json").exists());

    // Restore into a DIFFERENT fresh root — same shape as the unencrypted case, exercised
    // through the encrypted (today: identity-stub, per #19's scope; real `age` lands in
    // #23) code path instead.
    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output.clone());
    let restore_outcome = run_restore(&outcome.backup_dir, &restore_ctx).expect("restore should succeed");

    assert_round_trip_landed(&outcome, &restore_outcome, &home, &restore_home);
}

#[test]
fn backup_dir_collides_and_refuses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);

    let first = run_backup(&selection, &ctx, &Encryption::None);
    assert!(first.is_ok(), "first backup into a fresh Output location should succeed");

    // Same ctx (same host + timestamp) => same computed Backup dir name => must refuse
    // rather than silently overwrite (story 33).
    let second = run_backup(&selection, &ctx, &Encryption::None);
    match second {
        Err(CoreError::BackupDirExists(path)) => {
            assert_eq!(path, first.unwrap().backup_dir);
        }
        other => panic!("expected CoreError::BackupDirExists, got {other:?}"),
    }
}
