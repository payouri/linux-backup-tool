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
//! #19's original scope only exercised Shell (`catalog::SHELL`, `.oh-my-zsh/custom/` only).
//! #20 adds `taxonomy_round_trip_composes_multiple_categories` alongside it (not replacing
//! it — see that test's doc), proving the full ten-Category taxonomy's precedence + Smart
//! defaults + Hide-empty compose correctly through the same backup/restore seam.

// This crate is only ever built as a test binary (`--test`), so this allow is unconditional
// — AGENTS.md exempts test code from the no-unwrap/expect/panic rule.
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use secrecy::SecretString;

use linux_backup_core::archive::Encryption;
use linux_backup_core::context::{Privilege, RunContext};
use linux_backup_core::model::{DirectoryState, Entry, EntryRoot, EntryType, SelectionModel};
use linux_backup_core::packages::{CommandRunner, NoCommandRunner};
use linux_backup_core::progress::{NoopProgress, Phase, Progress};
use linux_backup_core::restore::{CollisionPolicy, RestoreOutcome};
use linux_backup_core::{catalog, crypto, manifest, scan, BackupOutcome, CancelToken, CoreError};

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
    fs::write(
        plugin_dir.join("my-plugin.plugin.zsh"),
        b"# my plugin\necho hi\n",
    )
    .expect("write plugin file");

    // Outside the Curated allowlist (Shell only covers oh-my-zsh/custom/, never the whole
    // framework) — must not be picked up by scan_category.
    let themes_dir = home.join(".oh-my-zsh/themes");
    fs::create_dir_all(&themes_dir).expect("create themes dir");
    fs::write(themes_dir.join("robbyrussell.zsh-theme"), b"# theme\n").expect("write theme file");

    // Distinct from the themes case above (same framework, wrong subdir): this file isn't
    // on *any* Category's allowlist, proving scan_category doesn't fall back to some
    // broader home-relative default when nothing more specific matches.
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

/// Runs `core::backup` with [`NoCommandRunner`] injected for package-list capture (#25):
/// the round-trip seam stays off the real-package-manager system boundary and fully
/// deterministic regardless of what's installed on whatever machine runs this suite — see
/// `packages::NoCommandRunner`'s doc.
fn run_backup(
    selection: &SelectionModel,
    ctx: &RunContext,
    encryption: &Encryption,
) -> Result<BackupOutcome, CoreError> {
    linux_backup_core::backup(
        selection,
        ctx,
        encryption,
        &NoCommandRunner,
        &mut NoopProgress,
        &CancelToken::new(),
    )
}

/// Runs `core::restore`. `passphrase` is `None` for an unencrypted Backup and
/// `Some(&passphrase)` for an encrypted one — see `restore::restore`'s doc for why the
/// signature carries it explicitly rather than reading it from the Backup dir itself
/// (encryption is never persisted, per CONTEXT.md's Profile entry). Uses a
/// [`CollisionPolicy::Confirm`] that declines any collision — every test on this helper
/// assumes a clean restore root, so a surfaced collision (skipped, never clobbered) would
/// show up as an unexpected `skipped_collisions` entry; the `restore_collision_*` tests
/// exercise the overwrite and confirm/decline paths directly.
fn run_restore(
    backup_dir: &Path,
    ctx: &RunContext,
    passphrase: Option<&SecretString>,
) -> Result<RestoreOutcome, CoreError> {
    let mut decline = |_: &[PathBuf]| false;
    linux_backup_core::restore::restore(
        backup_dir,
        ctx,
        passphrase,
        CollisionPolicy::Confirm(&mut decline),
        &mut NoopProgress,
        &CancelToken::new(),
    )
}

/// Builds a synthetic `$HOME` under `root` exercising more than one Category at once
/// (#20's taxonomy, composed on top of #19's single-Category seam):
/// - an SSH key (`catalog::SSH`, Sensitive, default-ON) alongside the Shell fixture files
///   from [`build_fixture_home`],
/// - a User-data file (`catalog::USER_DATA`, default-OFF) that must be discoverable by
///   `scan::scan_taxonomy` but must NOT make it into a Smart-defaults Selection.
///
/// Returns the Fixture environment's home path.
fn build_multi_category_fixture_home(root: &Path) -> PathBuf {
    let home = build_fixture_home(root);

    let ssh_dir = home.join(".ssh");
    fs::create_dir_all(&ssh_dir).expect("create .ssh dir");
    fs::write(ssh_dir.join("id_ed25519"), b"fake-private-key-bytes\n").expect("write id_ed25519");

    let documents_dir = home.join("Documents");
    fs::create_dir_all(&documents_dir).expect("create Documents dir");
    fs::write(documents_dir.join("report.pdf"), b"fake-pdf-bytes\n").expect("write report.pdf");

    home
}

/// Builds a synthetic `$HOME` under `root` exercising #21's Drill-down on top of #19's
/// single-Category Shell fixture: a KDE Plasma Category with three Entries —
/// `.config/kwinrc` (the Entry that must survive deselection) alongside one file each under
/// `.local/share/icons/` and `.local/share/wallpapers/` (story 13's concrete example: the
/// heavy Entries a user drills down to exclude). The fixture files stand in for the real
/// 226MB icon theme — Drill-down excludes by relative path regardless of size.
///
/// Returns the Fixture environment's home path.
fn build_kde_plasma_fixture_home(root: &Path) -> PathBuf {
    let home = build_fixture_home(root);

    fs::create_dir_all(home.join(".config")).expect("create .config dir");
    fs::write(home.join(".config/kwinrc"), b"[Windows]\nfake-kwinrc\n").expect("write kwinrc");

    let icons_dir = home.join(".local/share/icons/hicolor");
    fs::create_dir_all(&icons_dir).expect("create icons dir");
    fs::write(icons_dir.join("heavy-icon-theme.svg"), b"fake-icon-bytes\n")
        .expect("write heavy icon");

    let wallpapers_dir = home.join(".local/share/wallpapers/mywallpaper");
    fs::create_dir_all(&wallpapers_dir).expect("create wallpapers dir");
    fs::write(
        wallpapers_dir.join("1920x1080.jpg"),
        b"fake-wallpaper-bytes\n",
    )
    .expect("write wallpaper");

    home
}

/// Asserts a completed backup+restore round-trip landed correctly: no restore failures,
/// both fixture files restored byte-for-byte under `restore_home` (not the original
/// `original_home` — Re-rooting), and the out-of-allowlist file never appearing anywhere
/// in the Backup.
fn assert_round_trip_landed(
    outcome: &BackupOutcome,
    restore_outcome: &RestoreOutcome,
    original_home: &Path,
    restore_home: &Path,
) {
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
    let original_plugin =
        original_home.join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh");
    let restored_aliases = restore_home.join(".oh-my-zsh/custom/aliases.zsh");
    let restored_plugin =
        restore_home.join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh");

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
    assert!(
        archive_path.is_file(),
        "unencrypted Archive should have no .age suffix"
    );
    assert!(
        manifest_path.is_file(),
        "unencrypted Manifest should have no .age suffix"
    );
    assert!(!outcome.backup_dir.join("archive.tar.zst.age").exists());
    assert!(!outcome.backup_dir.join("manifest.json.age").exists());

    let written_manifest: manifest::Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap())
            .expect("manifest.json should parse");
    assert_eq!(
        written_manifest.schema_version,
        outcome.manifest.schema_version
    );

    let m = &outcome.manifest;
    assert_eq!(m.schema_version, manifest::SCHEMA_VERSION);
    assert_eq!(m.host, ctx.host);
    assert_eq!(m.user, ctx.user);
    assert_eq!(m.uid, ctx.uid);
    assert_eq!(m.os, ctx.os);
    assert_eq!(m.home, ctx.home);
    assert_eq!(m.created_at, "2023-11-14T22:13:20Z");
    // Empty because run_backup injects NoCommandRunner (#25), not because packages are
    // unimplemented — package-list parsing is unit-tested directly in `packages`.
    assert!(m.packages.is_empty());

    let archive_bytes_on_disk = fs::read(&archive_path).unwrap();
    // Unencrypted: the archive_sha256 in the Manifest is of the plaintext bytes, which
    // (with Encryption::None) are exactly what's on disk.
    assert_eq!(
        m.archive_sha256,
        linux_backup_core::archive::sha256_hex(&archive_bytes_on_disk)
    );

    // Restore into a DIFFERENT fresh root.
    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert_round_trip_landed(&outcome, &restore_outcome, &home, &restore_home);
}

/// #23: the encrypted path now runs through real `age::Encryptor::with_user_passphrase`
/// encryption (replacing #19's identity-stub coverage of the same shape) — both the Archive
/// and Manifest land as genuine age ciphertext, and restoring with the correct passphrase
/// recovers the plaintext byte-for-byte via the same round-trip assertions as the
/// unencrypted case.
#[test]
fn round_trip_encrypted_with_real_age_and_the_correct_passphrase() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let selection = shell_selection(&ctx);
    let passphrase = SecretString::from("correct-horse-battery-staple");
    let encryption = Encryption::Passphrase(passphrase.clone());

    let outcome = run_backup(&selection, &ctx, &encryption).expect("backup should succeed");

    // Story 23: the Backup dir's own name never changes shape because of encryption — it
    // stays readable without a passphrase, identical in format to the unencrypted case.
    let expected_dir_name = format!(
        "linux-backup-{}-{}",
        ctx.host,
        ctx.now.duration_since(UNIX_EPOCH).unwrap().as_secs()
    );
    assert_eq!(
        outcome.backup_dir.file_name().and_then(|n| n.to_str()),
        Some(expected_dir_name.as_str()),
        "the Backup dir name must stay readable without a passphrase, unaffected by encryption"
    );

    // Encrypted path: both the Archive and Manifest carry the .age suffix (CONTEXT.md:
    // encryption mirrors across both, all-or-nothing), and the unencrypted filenames must
    // not exist.
    let archive_path = outcome.backup_dir.join("archive.tar.zst.age");
    let manifest_path = outcome.backup_dir.join("manifest.json.age");
    assert!(
        archive_path.is_file(),
        "encrypted Archive should carry the .age suffix"
    );
    assert!(
        manifest_path.is_file(),
        "encrypted Manifest should carry the .age suffix"
    );
    assert!(!outcome.backup_dir.join("archive.tar.zst").exists());
    assert!(!outcome.backup_dir.join("manifest.json").exists());

    // The bytes on disk must be genuine age ciphertext, not plaintext, for both files —
    // proves this test would fail if `encrypt` regressed back to an identity stub.
    let archive_bytes_on_disk = fs::read(&archive_path).expect("read encrypted archive");
    let manifest_bytes_on_disk = fs::read(&manifest_path).expect("read encrypted manifest");
    // Real age ciphertext starts with the format's own "age-encryption.org/v1" header —
    // proves this is genuine age output, not a zstd frame (0x28 0xB5 0x2F 0xFD) or plaintext
    // JSON that slipped through unencrypted.
    assert_eq!(
        &archive_bytes_on_disk[..4],
        b"age-",
        "encrypted Archive bytes on disk should be real age ciphertext"
    );
    assert!(
        serde_json::from_slice::<manifest::Manifest>(&manifest_bytes_on_disk).is_err(),
        "the on-disk Manifest bytes must be ciphertext, not parseable plaintext JSON"
    );

    // Restore into a DIFFERENT fresh root, with the SAME correct passphrase.
    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome = run_restore(&outcome.backup_dir, &restore_ctx, Some(&passphrase))
        .expect("restore with the correct passphrase should succeed");

    assert_round_trip_landed(&outcome, &restore_outcome, &home, &restore_home);
}

/// Acceptance criterion: restoring an encrypted Backup with a WRONG passphrase must fail
/// clearly (a typed `CoreError::DecryptionFailed`, never a panic) and without partially
/// writing any files — restore reads and decrypts the Manifest before touching the
/// filesystem at all, so a decrypt failure there means `restore_home` is never even created.
#[test]
fn restore_with_wrong_passphrase_fails_without_partial_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = shell_selection(&ctx);
    let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));

    let outcome = run_backup(&selection, &ctx, &encryption).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let wrong_passphrase = SecretString::from("a-completely-different-passphrase");

    match run_restore(&outcome.backup_dir, &restore_ctx, Some(&wrong_passphrase)) {
        Err(CoreError::DecryptionFailed(_)) => {}
        other => panic!("expected CoreError::DecryptionFailed, got {other:?}"),
    }

    assert!(
        !restore_home.exists(),
        "a failed decrypt must leave no partially-restored files behind"
    );
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
    assert!(
        first.is_ok(),
        "first backup into a fresh Output location should succeed"
    );

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

/// Proves the full ten-Category taxonomy (#20) composes through the same seam #19 proved
/// for Shell alone: `scan::scan_taxonomy`'s precedence + Hide-empty feeds
/// `SelectionModel::from_smart_defaults`, and the resulting multi-Category Selection
/// backs up and restores exactly like a single-Category one.
///
/// Fixture has three kinds of content (see `build_multi_category_fixture_home`):
/// - Shell (default-ON) and SSH (Sensitive, default-ON) both have real matches and must
///   both land in the Backup;
/// - User data (default-OFF) has a real match too, but must be *discoverable* by the scan
///   without being *selected* by Smart defaults — proving default on/off, not just
///   Hide-empty, drives what a Smart-defaults Selection contains;
/// - Categories with zero matches on this fixture (GPG, Cloud creds, ...) must not appear
///   in the scan at all (Hide-empty), except Custom paths, which is exempt.
#[test]
fn taxonomy_round_trip_composes_multiple_categories() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_multi_category_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());

    let scanned = scan::scan_taxonomy(&ctx);
    let scanned_names: Vec<&str> = scanned.iter().map(|s| s.category.name).collect();
    assert!(scanned_names.contains(&"Shell"));
    assert!(scanned_names.contains(&"SSH"));
    assert!(
        scanned_names.contains(&"User data"),
        "User data has a real match and must be discoverable"
    );
    assert!(
        scanned_names.contains(&"Custom paths"),
        "Custom paths is exempt from Hide-empty"
    );
    assert!(
        !scanned_names.contains(&"GPG"),
        "GPG has zero matches on this fixture and must be hidden"
    );

    let selection = SelectionModel::from_smart_defaults(&scanned);
    let selected_categories: std::collections::BTreeSet<&str> =
        selection.entries().iter().map(|e| e.category).collect();
    assert_eq!(
        selected_categories,
        std::collections::BTreeSet::from(["Shell", "SSH"]),
        "Smart defaults must include the default-ON categories and exclude default-OFF User data"
    );
    assert!(
        selection.warnings().is_empty(),
        "User data isn't in the Smart-defaults Selection at all, so its size warning must not fire"
    );

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    // Three Entries: the two Shell fixture files plus the one SSH key — User data's
    // report.pdf must be absent because Smart defaults excluded it, not because it wasn't
    // found.
    assert_eq!(outcome.manifest.entries.len(), 3);
    let manifest_categories: std::collections::BTreeSet<&str> = outcome
        .manifest
        .entries
        .iter()
        .map(|e| e.category.as_str())
        .collect();
    assert_eq!(
        manifest_categories,
        std::collections::BTreeSet::from(["Shell", "SSH"])
    );

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(restore_outcome.restored_paths.len(), 3);
    assert_eq!(
        fs::read(restore_home.join(".ssh/id_ed25519")).expect("restored ssh key should exist"),
        fs::read(home.join(".ssh/id_ed25519")).expect("original ssh key should exist"),
    );
    assert!(
        !restore_home.join("Documents").exists(),
        "User data was never selected, so nothing should land under Documents on restore"
    );
}

/// Issue #24 / story 42: an empty Selection must bounce before any filesystem work at all —
/// no Backup dir created for a caller to clean up, distinct from every other failure mode on
/// this seam, which all fire after the dir already exists.
#[test]
fn empty_selection_is_refused_before_any_filesystem_work() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = SelectionModel::new(Vec::new());

    match run_backup(&selection, &ctx, &Encryption::None) {
        Err(CoreError::EmptySelection) => {}
        other => panic!("expected CoreError::EmptySelection, got {other:?}"),
    }

    assert!(
        fs::read_dir(&output)
            .expect("output location should still exist")
            .next()
            .is_none(),
        "an empty Selection must not create a Backup dir at all"
    );
}

/// Issue #35 / stories 6/8/62: a completely empty account (no discoverable Categories) adds a
/// Custom path and completes a backup end-to-end. Smart defaults leave the Selection empty —
/// which would bounce (story 42) — until `SelectionModel::add_custom_path` (the exact seam the
/// WHAT screen drives once `catalog::validate_custom_path` accepts a path) adds one. The
/// home-rooted Custom path must land in the Manifest under the Custom-paths Category and Re-root
/// under the *restoring* home, proving a Custom-path Entry round-trips like any other.
#[test]
fn empty_account_adds_a_custom_path_and_round_trips_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    // A file on no Category's Curated allowlist — reachable only as a Custom path (story 8).
    let notes = home.join("projects/notes.md");
    fs::create_dir_all(notes.parent().unwrap()).expect("create custom-path parent dir");
    fs::write(&notes, b"custom-path notes\n").expect("write custom-path file");
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());

    // An empty account: only the always-shown Custom-paths Category scans (exempt from
    // Hide-empty), so Smart defaults produce an empty Selection.
    let scanned = scan::scan_taxonomy(&ctx);
    let mut selection = SelectionModel::from_smart_defaults(&scanned);
    assert!(
        selection.is_empty(),
        "an empty account has nothing selected by Smart defaults"
    );

    // The path is under $HOME, so a non-root run accepts it; then it enters the Selection
    // through the same public API the WHAT screen calls (story 62).
    catalog::validate_custom_path(&notes, &ctx.home, ctx.privilege)
        .expect("a Custom path under $HOME is accepted for a non-root run");
    selection.add_custom_path(&notes, &ctx.home);
    assert!(
        !selection.is_empty(),
        "the added Custom path fills the Selection"
    );

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    assert_eq!(outcome.manifest.entries.len(), 1);
    let manifest_entry = &outcome.manifest.entries[0];
    assert_eq!(manifest_entry.category, catalog::CUSTOM_PATHS.name);
    assert_eq!(manifest_entry.path, "projects/notes.md");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(restore_outcome.restored_paths.len(), 1);
    // Re-rooting: the home-tagged Custom path lands under the *restoring* home, byte-for-byte.
    assert_ne!(restore_home, home);
    assert_eq!(
        fs::read(restore_home.join("projects/notes.md"))
            .expect("restored custom file should exist"),
        fs::read(&notes).expect("original custom file should exist"),
    );
}

/// Issue #24 / ADR-0002: a genuine read error (permission-denied, I/O error, device-gone) is
/// fatal and distinct from a Vanished entry — the whole run aborts and the freshly-created
/// Backup dir is deleted, so nothing (including no `manifest.json`) is left behind.
#[test]
fn hard_read_error_aborts_run_and_leaves_no_backup_dir_or_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let mut entries: Vec<Entry> = shell_selection(&ctx)
        .entries()
        .into_iter()
        .cloned()
        .collect();

    // A directory can't be read as file bytes: `fs::read` fails with something other than
    // `NotFound` (e.g. `IsADirectory`) — exactly the "real read error" ADR-0002 says is
    // fatal. Deterministic and portable, unlike a permission-denied probe that a root-run
    // test suite would sail straight through.
    let broken_dir = home.join("broken-entry-is-a-directory");
    fs::create_dir_all(&broken_dir).expect("create broken entry directory");
    entries.push(Entry {
        absolute_path: broken_dir,
        // Typed as a File though it points at a directory: `archive::build` then tries to
        // read it as bytes and hits a non-ENOENT error — exactly the "real read error"
        // ADR-0002 says is fatal.
        entry_type: EntryType::File,
        relative_path: PathBuf::from("broken-entry-is-a-directory"),
        root: EntryRoot::Home,
        category: catalog::SHELL.name,
        sensitive: false,
    });
    let selection = SelectionModel::new(entries);

    match run_backup(&selection, &ctx, &Encryption::None) {
        Err(CoreError::ReadFailed { .. }) => {}
        other => panic!("expected CoreError::ReadFailed, got {other:?}"),
    }

    let expected_dir_name = format!(
        "linux-backup-{}-{}",
        ctx.host,
        ctx.now.duration_since(UNIX_EPOCH).unwrap().as_secs()
    );
    let backup_dir = output.join(expected_dir_name);
    assert!(
        !backup_dir.exists(),
        "an aborted backup run must leave no Backup dir behind"
    );
    assert!(
        !backup_dir.join("manifest.json").exists(),
        "no Manifest may be written for a run that aborts"
    );
}

/// Issue #24 / CONTEXT.md's "Vanished": an Entry that existed at scan time but is gone
/// (ENOENT) by read time is benign — skipped-with-note, the run still completes, and the
/// gap shows up in `manifest.vanished` rather than aborting like a real read error would.
#[test]
fn vanished_benign_file_is_skipped_and_recorded_without_aborting() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    let vanishing_entry = selection
        .entries()
        .into_iter()
        .find(|e| e.relative_path.ends_with("aliases.zsh"))
        .expect("aliases.zsh should be in the Shell selection")
        .clone();
    let expected_archive_path = vanishing_entry
        .archive_path()
        .to_string_lossy()
        .into_owned();

    // Simulate the scan-to-read race: the file existed when `shell_selection` scanned above,
    // but is gone by the time `backup` gets around to reading it.
    fs::remove_file(&vanishing_entry.absolute_path).expect("delete file to simulate vanished");

    let outcome = run_backup(&selection, &ctx, &Encryption::None)
        .expect("a Vanished entry must not abort the run");

    // Both Shell files remain listed as selected roots (Selection metadata doesn't change),
    // but only the still-present plugin file has an actual checksum.
    assert_eq!(outcome.manifest.entries.len(), 2);
    assert_eq!(outcome.manifest.checksums.len(), 1);
    assert_eq!(outcome.manifest.vanished.len(), 1);
    assert_eq!(outcome.manifest.vanished[0].path, expected_archive_path);
    assert!(
        !outcome.manifest.vanished[0].sensitive,
        "Shell is not a Sensitive Category"
    );
    assert!(
        outcome.manifest.vanished_sensitive_names().is_empty(),
        "a non-sensitive Vanished entry must not be called out as sensitive"
    );
}

/// Issue #24 / story 40: a Vanished entry that was also Sensitive must be nameable by
/// `vanished_sensitive_names()` for a later run-summary caller (TUI) to call out — distinct
/// from a non-sensitive Vanished entry, which is recorded but not specially flagged.
#[test]
fn vanished_sensitive_entry_is_named_by_vanished_sensitive_names() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_multi_category_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let scanned = scan::scan_taxonomy(&ctx);
    let selection = SelectionModel::from_smart_defaults(&scanned);

    let ssh_key_entry = selection
        .entries()
        .into_iter()
        .find(|e| e.category == catalog::SSH.name)
        .expect("SSH key should be in the Smart-defaults Selection")
        .clone();
    assert!(ssh_key_entry.sensitive, "SSH is a Sensitive Category");
    let expected_archive_path = ssh_key_entry.archive_path().to_string_lossy().into_owned();

    // Simulate the scan-to-read race for a Sensitive entry specifically.
    fs::remove_file(&ssh_key_entry.absolute_path).expect("delete ssh key to simulate vanished");

    let outcome = run_backup(&selection, &ctx, &Encryption::None)
        .expect("a Vanished entry, even a Sensitive one, must not abort the run");

    assert_eq!(
        outcome.manifest.vanished_sensitive_names(),
        vec![expected_archive_path.as_str()]
    );
}

/// Builds a synthetic `$HOME` under `root` exercising #22's Safety-net scan on top of
/// #19's single-Category Shell fixture: a `.config/git/credentials` file. `.config/git` is
/// on the **Program configs** allowlist (`catalog::PROGRAM_CONFIGS`, `sensitive: false`),
/// so this file's Sensitivity can only come from the Safety-net scan matching "credentials"
/// against the "credential" keyword (`scan::is_safety_net_match`) — never from its Category.
///
/// Returns the Fixture environment's home path.
fn build_safety_net_fixture_home(root: &Path) -> PathBuf {
    let home = build_fixture_home(root);

    let git_dir = home.join(".config/git");
    fs::create_dir_all(&git_dir).expect("create .config/git dir");
    fs::write(git_dir.join("credentials"), b"fake-git-credential-bytes\n")
        .expect("write credentials file");

    home
}

/// Proves #22's Safety-net scan composes through the full backup/restore seam: an Entry
/// whose Category is not itself Sensitive (Program configs) still ends up `sensitive: true`
/// end-to-end — in the Selection, in the written Manifest, and in `crypto::evaluate`'s
/// Auto-insist policy, which must in turn report `encryption_required: true` and name
/// "Program configs" as a triggering Category even though it isn't a sensitive Category by
/// declaration.
#[test]
fn safety_net_only_entry_flows_sensitive_through_the_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_safety_net_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let scanned = scan::scan_taxonomy(&ctx);
    let selection = SelectionModel::from_smart_defaults(&scanned);

    let credentials_entry = selection
        .entries()
        .into_iter()
        .find(|e| e.relative_path == Path::new(".config/git/credentials"))
        .expect("credentials file should be selected via Program configs' Smart default");
    assert_eq!(credentials_entry.category, "Program configs");
    assert!(
        credentials_entry.sensitive,
        "Safety-net match must flag this Entry Sensitive even though Program configs isn't \
         a sensitive Category"
    );

    let policy = crypto::evaluate(&selection);
    assert!(
        policy.encryption_required,
        "a Safety-net-only match must still Auto-insist on encryption"
    );
    assert!(
        policy.sensitive_categories.contains(&"Program configs"),
        "Auto-insist must name the triggering Category by name, even though it isn't a \
         declared-sensitive one: {:?}",
        policy.sensitive_categories
    );
    assert!(policy
        .sensitive_entries
        .contains(&PathBuf::from(".config/git/credentials")));

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");
    let manifest_entry = outcome
        .manifest
        .entries
        .iter()
        .find(|e| e.path == ".config/git/credentials")
        .expect("credentials file should be recorded in the Manifest");
    assert!(
        manifest_entry.sensitive,
        "the written Manifest must also carry the Safety-net-derived sensitivity"
    );

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");
    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(
        fs::read(restore_home.join(".config/git/credentials"))
            .expect("restored credentials file should exist"),
        fs::read(home.join(".config/git/credentials"))
            .expect("original credentials file should exist"),
    );
}

/// The negative case for #22: a Selection with no sensitive Category Entries and no
/// Safety-net matches must leave `encryption_required` false (the last of #22's four
/// acceptance criteria).
#[test]
fn selection_with_no_sensitive_or_safety_net_matches_does_not_require_encryption() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);

    let policy = crypto::evaluate(&selection);
    assert!(!policy.encryption_required);
    assert!(policy.sensitive_categories.is_empty());
    assert!(policy.sensitive_entries.is_empty());
    assert!(!selection.encryption_required());
}

/// Issue #21 / stories 12-13: proves Drill-down deselection through the full backup/restore
/// seam — deselecting KDE Plasma's `icons/`/`wallpapers/` Entries excludes exactly those two
/// from the Manifest and Archive while `kwinrc` (the rest of the otherwise-checked Category)
/// still lands normally, and the Category-level Smart default is never touched to do it.
#[test]
fn kde_plasma_drill_down_deselection_excludes_exactly_those_entries_from_the_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_kde_plasma_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let scanned = scan::scan_taxonomy(&ctx);
    let mut selection = SelectionModel::from_smart_defaults(&scanned);

    // KDE Plasma is a Smart-default-ON Category (story 3's siblings), so all three fixture
    // Entries start in the active Selection before any Drill-down toggling.
    let plasma_entries_before: Vec<PathBuf> = selection
        .entries()
        .into_iter()
        .filter(|e| e.category == catalog::KDE_PLASMA.name)
        .map(|e| e.relative_path.clone())
        .collect();
    assert_eq!(
        plasma_entries_before.len(),
        3,
        "all three KDE Plasma fixture Entries should start selected: {plasma_entries_before:?}"
    );

    let icon_relative_path = PathBuf::from(".local/share/icons/hicolor/heavy-icon-theme.svg");
    let wallpaper_relative_path =
        PathBuf::from(".local/share/wallpapers/mywallpaper/1920x1080.jpg");
    let kwinrc_relative_path = PathBuf::from(".config/kwinrc");

    selection.deselect_entry(&icon_relative_path);
    selection.deselect_entry(&wallpaper_relative_path);

    // The Category-level checkbox is untouched by Drill-down: kwinrc is still selected, and
    // Drill-down listing (`entries_in_category`) still surfaces all three Entries so a UI
    // could re-toggle the withheld ones — only the *active* Selection shrank.
    assert!(selection
        .entries()
        .into_iter()
        .any(|e| e.relative_path == kwinrc_relative_path));
    assert!(!selection
        .entries()
        .into_iter()
        .any(|e| e.relative_path == icon_relative_path));
    assert!(!selection
        .entries()
        .into_iter()
        .any(|e| e.relative_path == wallpaper_relative_path));
    assert_eq!(
        selection
            .entries_in_category(catalog::KDE_PLASMA.name)
            .len(),
        3,
        "Drill-down listing must still show every Entry, deselected or not"
    );
    assert!(selection.is_deselected(&icon_relative_path));
    assert!(selection.is_deselected(&wallpaper_relative_path));
    assert_eq!(
        selection.deselected_paths(),
        &std::collections::BTreeSet::from([
            icon_relative_path.clone(),
            wallpaper_relative_path.clone()
        ])
    );

    // The Drill-down explorer's tri-state (#48) is derived from this same withheld state, so it
    // must agree with what the backup below actually excludes: the two deselected subtrees read
    // None, the still-selected `.config` reads All, and their shared Category is Partial overall.
    assert_eq!(
        selection.directory_state(Path::new(".local/share/icons")),
        DirectoryState::None
    );
    assert_eq!(
        selection.directory_state(Path::new(".local/share/wallpapers")),
        DirectoryState::None
    );
    assert_eq!(
        selection.directory_state(Path::new(".config")),
        DirectoryState::All
    );

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    // Only kwinrc (KDE Plasma) plus the two Shell fixture files land — the deselected icon
    // and wallpaper Entries never reach the Manifest or Archive at all.
    let manifest_paths: std::collections::BTreeSet<&str> = outcome
        .manifest
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert!(manifest_paths.contains(".config/kwinrc"));
    assert!(!manifest_paths.contains(icon_relative_path.to_str().unwrap()));
    assert!(!manifest_paths.contains(wallpaper_relative_path.to_str().unwrap()));
    assert!(outcome.manifest.checksums.keys().all(
        |archive_path| !archive_path.contains("icons") && !archive_path.contains("wallpapers")
    ));

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(
        fs::read(restore_home.join(".config/kwinrc")).expect("restored kwinrc should exist"),
        fs::read(home.join(".config/kwinrc")).expect("original kwinrc should exist"),
    );
    assert!(
        !restore_home.join(".local/share/icons").exists(),
        "deselected icons/ Entry must never land on restore"
    );
    assert!(
        !restore_home.join(".local/share/wallpapers").exists(),
        "deselected wallpapers/ Entry must never land on restore"
    );
}

/// Issue #38 / story 13: proves *root-level* Drill-down deselection through the full
/// backup/restore seam — deselecting KDE Plasma's `.local/share/icons` and
/// `.local/share/wallpapers` allowlist roots as whole units (not each expanded file) keeps
/// their entire subtrees out of the Manifest, Archive, and restore, while `kwinrc` (the rest
/// of the otherwise-checked Category) still lands and the Category-level Smart default is
/// never touched. This is the acceptance test for dropping the ~226MB icon theme in one action.
#[test]
fn kde_plasma_root_level_deselection_excludes_the_whole_subtree_from_the_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_kde_plasma_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home.clone(), output.clone());
    let scanned = scan::scan_taxonomy(&ctx);
    let mut selection = SelectionModel::from_smart_defaults(&scanned);

    let icons_root = PathBuf::from(".local/share/icons");
    let wallpapers_root = PathBuf::from(".local/share/wallpapers");

    // One toggle per heavy directory root drops its whole subtree — the point of #38.
    selection.deselect_root(&icons_root);
    selection.deselect_root(&wallpapers_root);

    // The Category-level checkbox is untouched: kwinrc stays selected (partial, not off), and
    // the roots are recorded as two root paths — not the individual files they expand to.
    assert!(selection
        .entries()
        .into_iter()
        .any(|e| e.relative_path == Path::new(".config/kwinrc")));
    assert!(!selection.is_empty());
    assert!(selection.is_root_deselected(&icons_root));
    assert!(selection.is_root_deselected(&wallpapers_root));
    assert!(
        selection.deselected_paths().is_empty(),
        "a root deselection records roots, never each expanded file"
    );
    assert_eq!(
        selection.deselected_roots(),
        &std::collections::BTreeSet::from([icons_root.clone(), wallpapers_root.clone()])
    );

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let manifest_paths: std::collections::BTreeSet<&str> = outcome
        .manifest
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert!(manifest_paths.contains(".config/kwinrc"));
    assert!(
        manifest_paths
            .iter()
            .all(|p| !p.contains("icons") && !p.contains("wallpapers")),
        "no Entry under a deselected root may reach the Manifest: {manifest_paths:?}"
    );
    assert!(outcome.manifest.checksums.keys().all(
        |archive_path| !archive_path.contains("icons") && !archive_path.contains("wallpapers")
    ));

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );
    assert_eq!(
        fs::read(restore_home.join(".config/kwinrc")).expect("restored kwinrc should exist"),
        fs::read(home.join(".config/kwinrc")).expect("original kwinrc should exist"),
    );
    assert!(
        !restore_home.join(".local/share/icons").exists(),
        "the deselected icons/ root must never land on restore"
    );
    assert!(
        !restore_home.join(".local/share/wallpapers").exists(),
        "the deselected wallpapers/ root must never land on restore"
    );
}

/// A [`CommandRunner`] returning fixed, canned stdout per program — unlike
/// `packages::NoCommandRunner` (every manager absent, used by every other test on this
/// seam), this lets a test exercise `reinstall.sh` generation against Manifest Package
/// lists that were actually captured (#25), without touching real package managers.
struct FakeCommandRunner {
    pacman: Option<&'static str>,
    npm: Option<&'static str>,
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        match (program, args) {
            ("pacman", ["-Qen"]) => self.pacman.map(str::to_string),
            ("npm", ["list", "-g", "--depth=0", "--json"]) => self.npm.map(str::to_string),
            _ => None,
        }
    }
}

/// Issue #26/#37 / story 54: an existing file at a restore target must never be silently
/// clobbered. `core` routes the decision through [`CollisionPolicy`] — declining a
/// `Confirm` (or the equivalent, the user saying "no" to `tui`'s single prompt) leaves the
/// file untouched and records it in `skipped_collisions` (a deliberate skip, *not* a
/// failure), while `Confirm` accepting or `Overwrite` replaces it. Distinct from the
/// Backup-dir-name collision (#19/#27), which guards where the *Backup itself* is written.
#[test]
fn restore_collision_is_skipped_on_decline_and_overwritten_on_confirm_or_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = shell_selection(&ctx);
    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let conflicting_path = restore_home.join(".oh-my-zsh/custom/aliases.zsh");
    fs::create_dir_all(conflicting_path.parent().unwrap())
        .expect("create pre-existing conflicting dir");
    fs::write(&conflicting_path, b"pre-existing content that must survive")
        .expect("write pre-existing conflicting file");

    let restore_ctx = test_ctx(restore_home.clone(), output);

    // Declining the confirmation: the collision is recorded as a skip (not a failure), the
    // pre-existing content is left exactly as it was, and the prompt is asked once naming the
    // colliding target.
    let mut prompted_with: Vec<PathBuf> = Vec::new();
    let mut decline = |collisions: &[PathBuf]| {
        prompted_with.extend_from_slice(collisions);
        false
    };
    let outcome_declined = linux_backup_core::restore::restore(
        &outcome.backup_dir,
        &restore_ctx,
        None,
        CollisionPolicy::Confirm(&mut decline),
        &mut NoopProgress,
        &CancelToken::new(),
    )
    .expect("restore should still complete despite one declined collision (ADR-0002)");

    assert_eq!(
        prompted_with,
        vec![conflicting_path.clone()],
        "the single confirmation must name exactly the colliding target"
    );
    assert_eq!(
        fs::read(&conflicting_path).expect("conflicting file should still exist"),
        b"pre-existing content that must survive"
    );
    assert!(
        outcome_declined
            .failures
            .iter()
            .all(|f| f.path != conflicting_path.display().to_string()),
        "a declined collision must NOT be reported as a failure: {:?}",
        outcome_declined.failures
    );
    assert_eq!(
        outcome_declined
            .skipped_collisions
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec![conflicting_path.display().to_string().as_str()],
        "a declined collision must be reported as a skip"
    );
    // The other fixture file, with no collision, must still land normally — one declined
    // collision must not abort the rest of the run (ADR-0002).
    assert_eq!(
        fs::read(restore_home.join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh"))
            .expect("non-colliding file should still be restored"),
        fs::read(
            tmp.path()
                .join("home")
                .join(".oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh")
        )
        .expect("original plugin file should exist")
    );

    // Confirming the overwrite: the same restore now replaces the conflicting file with the
    // Backup's content, and nothing is left skipped.
    let mut accept = |_: &[PathBuf]| true;
    let outcome_confirmed = linux_backup_core::restore::restore(
        &outcome.backup_dir,
        &restore_ctx,
        None,
        CollisionPolicy::Confirm(&mut accept),
        &mut NoopProgress,
        &CancelToken::new(),
    )
    .expect("restore should succeed once the overwrite is confirmed");
    assert!(
        outcome_confirmed.skipped_collisions.is_empty(),
        "confirming the overwrite must leave nothing skipped: {:?}",
        outcome_confirmed.skipped_collisions
    );
    assert_eq!(
        fs::read(&conflicting_path).expect("file should exist after overwrite"),
        fs::read(tmp.path().join("home/.oh-my-zsh/custom/aliases.zsh"))
            .expect("original aliases.zsh should exist"),
        "a confirmed overwrite must let the Backup's content replace the pre-existing file"
    );

    // Restore the collision, then prove `--overwrite` (CollisionPolicy::Overwrite) clobbers
    // non-interactively with no prompt callback at all — the unchanged AFK path.
    fs::write(&conflicting_path, b"pre-existing content that must survive")
        .expect("re-create the collision");
    let outcome_flag = linux_backup_core::restore::restore(
        &outcome.backup_dir,
        &restore_ctx,
        None,
        CollisionPolicy::Overwrite,
        &mut NoopProgress,
        &CancelToken::new(),
    )
    .expect("restore with --overwrite should succeed");
    assert!(
        outcome_flag.skipped_collisions.is_empty(),
        "--overwrite must leave nothing skipped: {:?}",
        outcome_flag.skipped_collisions
    );
    assert_eq!(
        fs::read(&conflicting_path).expect("file should exist after overwrite"),
        fs::read(tmp.path().join("home/.oh-my-zsh/custom/aliases.zsh"))
            .expect("original aliases.zsh should exist"),
        "--overwrite must let the Backup's content replace the pre-existing file"
    );
}

/// Issue #26 / story 56: `reinstall.sh` is written from the Manifest's actual captured
/// Package lists (#25) — unpinned, correct per-manager counts, never executed by anything
/// in `core`.
#[test]
fn reinstall_script_is_written_with_correct_per_manager_counts_from_captured_packages() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = shell_selection(&ctx);

    let runner = FakeCommandRunner {
        pacman: Some("acpi 1.8-2\nripgrep 13.0.0-1\n"),
        npm: Some(r#"{"dependencies": {"typescript": {"version": "4.8.3"}}}"#),
    };
    let outcome = linux_backup_core::backup(
        &selection,
        &ctx,
        &Encryption::None,
        &runner,
        &mut NoopProgress,
        &CancelToken::new(),
    )
    .expect("backup should succeed");

    assert_eq!(
        outcome.manifest.packages[linux_backup_core::packages::PACMAN].len(),
        2
    );
    assert_eq!(
        outcome.manifest.packages[linux_backup_core::packages::NPM].len(),
        1
    );

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert_eq!(
        restore_outcome.package_counts[linux_backup_core::packages::PACMAN],
        2
    );
    assert_eq!(
        restore_outcome.package_counts[linux_backup_core::packages::NPM],
        1
    );

    assert_eq!(
        restore_outcome.reinstall_script_path,
        restore_home.join("reinstall.sh")
    );
    let script = fs::read_to_string(&restore_outcome.reinstall_script_path)
        .expect("reinstall.sh should have been written");
    assert!(script.contains("sudo pacman -S --needed acpi ripgrep"));
    assert!(script.contains("npm install -g typescript"));
    // Story 48/56: never pin captured versions.
    assert!(!script.contains("1.8-2") && !script.contains("13.0.0") && !script.contains("4.8.3"));
}

/// Issue #26 / story 57: the opt-in `--verify` pass (the underlying `core` function; wiring
/// an actual CLI flag is #29's job) must catch a restored file that was tampered with after
/// a successful restore.
#[test]
fn verify_pass_flags_a_deliberately_corrupted_restored_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = shell_selection(&ctx);
    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");
    assert!(restore_outcome.failures.is_empty());

    // Tamper with one restored file after a successful restore.
    let tampered = restore_home.join(".oh-my-zsh/custom/aliases.zsh");
    fs::write(
        &tampered,
        b"tampered bytes that do not match the Manifest checksum",
    )
    .expect("tamper with restored file");

    let report = linux_backup_core::restore::verify(&restore_ctx, &restore_outcome.manifest);

    assert!(!report.is_clean());
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(
        report.mismatched[0].path,
        "home/.oh-my-zsh/custom/aliases.zsh"
    );
    assert!(report.missing.is_empty());
    // The other, untouched restored file must still verify clean.
    assert!(report
        .matched
        .contains(&"home/.oh-my-zsh/custom/plugins/my-plugin/my-plugin.plugin.zsh".to_string()));
}

/// A [`Progress`] recorder that captures the phase sequence and the last per-Entry count
/// `backup` reported, so #33's progress wiring can be asserted through the same public
/// backup seam rather than by inspecting `core`'s internals.
#[derive(Default)]
struct RecordingProgress {
    phases: Vec<Phase>,
    last_entry: Option<(usize, usize)>,
    entry_ticks: usize,
}

impl Progress for RecordingProgress {
    fn on_phase(&mut self, phase: Phase) {
        self.phases.push(phase);
    }

    fn on_entry(&mut self, done: usize, total: usize) {
        self.entry_ticks += 1;
        self.last_entry = Some((done, total));
    }
}

/// Issue #33/#65 / story 37/44: `backup` drives its injected [`Progress`] through the real
/// phase boundaries in order — `Scanning` (the per-Entry archive-read loop, which also ticks
/// `on_entry` once per Entry up to the total), then `Compressing`, then `Finalizing` — so
/// `crates/tui` can render live per-phase progress from a purely observational callback. A
/// plaintext backup emits **no** `Encrypting` phase (#65: it is omitted entirely when the
/// backup isn't encrypted).
#[test]
fn backup_drives_progress_through_the_phases_in_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    // The Shell fixture has exactly two on-allowlist Entries.
    assert_eq!(selection.entries().len(), 2);

    let mut progress = RecordingProgress::default();
    let outcome = linux_backup_core::backup(
        &selection,
        &ctx,
        &Encryption::None,
        &NoCommandRunner,
        &mut progress,
        &CancelToken::new(),
    )
    .expect("backup should succeed");
    assert!(outcome.backup_dir.is_dir());

    assert_eq!(
        progress.phases,
        vec![Phase::Scanning, Phase::Compressing, Phase::Finalizing],
        "a plaintext backup must fire Scanning -> Compressing -> Finalizing, with no Encrypting"
    );
    assert!(
        !progress.phases.contains(&Phase::Encrypting),
        "a plaintext backup must omit the Encrypting phase entirely (#65)"
    );
    // on_entry ticks once per Entry and reaches (total, total) on the last one.
    assert_eq!(progress.entry_ticks, 2);
    assert_eq!(progress.last_entry, Some((2, 2)));
}

/// Issue #65 / story 44: an **encrypted** backup inserts the `Encrypting` phase between
/// `Compressing` and `Finalizing` — the one phase difference from the plaintext run above,
/// present precisely because there is real encryption work to report.
#[test]
fn encrypted_backup_inserts_the_encrypting_phase() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));

    let mut progress = RecordingProgress::default();
    linux_backup_core::backup(
        &selection,
        &ctx,
        &encryption,
        &NoCommandRunner,
        &mut progress,
        &CancelToken::new(),
    )
    .expect("backup should succeed");

    assert_eq!(
        progress.phases,
        vec![
            Phase::Scanning,
            Phase::Compressing,
            Phase::Encrypting,
            Phase::Finalizing
        ],
        "an encrypted backup must fire Scanning -> Compressing -> Encrypting -> Finalizing"
    );
}

/// Issue #65 / story 50: `restore` drives the same injected [`Progress`] channel `backup`
/// does (ADR-0005 D4's Progress-param extension) — `VerifyingArchive` (the pre-flight
/// checksum pass) then `Restoring`, ticking `on_entry` once per unpacked Entry. Exercised by
/// extending the restore half of this seam rather than adding a new one (spec #62 Testing
/// Decisions).
#[test]
fn restore_drives_progress_through_verify_then_restore_phases() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home, ctx.output_location);
    let mut progress = RecordingProgress::default();
    let mut decline = |_: &[PathBuf]| false;
    let restore_outcome = linux_backup_core::restore::restore(
        &outcome.backup_dir,
        &restore_ctx,
        None,
        CollisionPolicy::Confirm(&mut decline),
        &mut progress,
        &CancelToken::new(),
    )
    .expect("restore should succeed");
    assert_eq!(restore_outcome.restored_paths.len(), 2);

    assert_eq!(
        progress.phases,
        vec![Phase::VerifyingArchive, Phase::Restoring],
        "restore must fire VerifyingArchive then Restoring, in order"
    );
    // The two Shell fixture files unpack as two tar entries, ticking on_entry twice against
    // the Manifest's Entry count as the (approximate) total.
    assert_eq!(progress.entry_ticks, 2);
    assert_eq!(progress.last_entry, Some((2, 2)));
}

/// A [`Progress`] that flips a [`CancelToken`] the moment the first Entry is processed — the
/// only way, from a single-threaded test, to observe cancellation *mid-run* (after some work
/// has landed) rather than before it starts. `on_entry` fires just after the loop's top-of-body
/// cancel check for an Entry and just before that Entry's own work, so cancelling here lets the
/// current Entry finish and makes the *next* iteration's check stop the loop — yielding a
/// genuinely partial run.
struct CancelOnFirstEntry {
    cancel: CancelToken,
    entry_ticks: usize,
}

impl Progress for CancelOnFirstEntry {
    fn on_entry(&mut self, _done: usize, _total: usize) {
        self.entry_ticks += 1;
        if self.entry_ticks == 1 {
            self.cancel.cancel();
        }
    }
}

/// Issue #98 (GUI prefactor #97): cancelling a backup in its front-half scan loop aborts with
/// [`CoreError::Cancelled`] and, riding ADR-0002 fail-fast cleanup, leaves nothing on disk — a
/// cancel there always precedes the first on-disk byte, so the freshly-created Backup dir is
/// removed. Uses an already-cancelled token so the very first Entry's checkpoint trips.
#[test]
fn backup_cancelled_in_scan_loop_aborts_with_cancelled_and_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output.clone());
    let selection = shell_selection(&ctx);

    let cancel = CancelToken::new();
    cancel.cancel();
    let result = linux_backup_core::backup(
        &selection,
        &ctx,
        &Encryption::None,
        &NoCommandRunner,
        &mut NoopProgress,
        &cancel,
    );

    match result {
        Err(CoreError::Cancelled) => {}
        other => panic!("expected Err(CoreError::Cancelled), got {other:?}"),
    }
    // The Backup dir `backup` would have computed must not survive a cancel (fail-fast cleanup):
    // an aborted backup leaves the Output location exactly as it found it.
    let backup_dir = output.join(linux_backup_core::backup_dir_name(&ctx));
    assert!(
        !backup_dir.exists(),
        "a cancelled backup must remove the freshly-created Backup dir, leaving nothing behind"
    );
}

/// Issue #98 (GUI prefactor #97): cancelling a restore mid-unpack returns `Ok` with a *partial*
/// [`RestoreOutcome`] marked `cancelled` — restore has no cancel-safe front half (each unpacked
/// file is a commit, ADR-0002, no rollback), so it reports what landed rather than erroring.
/// The two-file Shell fixture, cancelled on its first Entry, restores exactly one file and stops.
#[test]
fn restore_cancelled_mid_unpack_returns_partial_outcome_marked_cancelled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    assert_eq!(
        selection.entries().len(),
        2,
        "fixture must have two Entries"
    );
    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home, ctx.output_location);
    let cancel = CancelToken::new();
    let mut progress = CancelOnFirstEntry {
        cancel: cancel.clone(),
        entry_ticks: 0,
    };
    let mut decline = |_: &[PathBuf]| false;
    let restore_outcome = linux_backup_core::restore::restore(
        &outcome.backup_dir,
        &restore_ctx,
        None,
        CollisionPolicy::Confirm(&mut decline),
        &mut progress,
        &cancel,
    )
    .expect("a cancelled restore reports partial success, never an error");

    assert!(
        restore_outcome.cancelled,
        "a restore stopped mid-unpack must mark the outcome cancelled"
    );
    assert_eq!(
        restore_outcome.restored_paths.len(),
        1,
        "cancelling on the first Entry lets that one land, then stops before the second"
    );
}

/// The `cancelled` marker is `false` for a restore that runs to completion — the never-cancelled
/// path, guarding against the marker being spuriously set.
#[test]
fn uncancelled_restore_reports_not_cancelled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = build_fixture_home(tmp.path());
    let output = tmp.path().join("output");
    fs::create_dir_all(&output).expect("create output dir");

    let ctx = test_ctx(home, output);
    let selection = shell_selection(&ctx);
    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home, ctx.output_location);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");

    assert!(
        !restore_outcome.cancelled,
        "a restore that ran to completion must not be marked cancelled"
    );
    assert_eq!(restore_outcome.restored_paths.len(), 2);
}

/// Issue #36 / stories 45+43: a symlink Entry survives the backup/restore round-trip AS a
/// symlink — its target path preserved verbatim, never dereferenced to the target's bytes —
/// and the Manifest records its `type` as `symlink`. The pre-fix archive step read every
/// Entry with a symlink-following `fs::read`, silently storing a copy of the target's
/// contents and restoring a plain file; this pins the corrected behavior down.
#[test]
fn round_trip_preserves_a_symlink_entry_and_records_its_type() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let output = tmp.path().join("output");
    fs::create_dir_all(home.join(".config")).expect("create .config dir");
    fs::create_dir_all(&output).expect("create output dir");

    // A real target file the symlink points at. If `build` regressed to dereferencing, the
    // Backup would carry a copy of THESE bytes as a regular file rather than the link — so a
    // distinctive target makes that failure mode unambiguous.
    fs::write(home.join(".config/target.txt"), b"i am the link target\n").expect("write target");

    // A relative symlink `.config/mylink -> target.txt`. Relative so the recreated link is
    // meaningful under the *restoring* home too (Re-rooting), not tied to the original path.
    let link_target = PathBuf::from("target.txt");
    let link_path = home.join(".config/mylink");
    std::os::unix::fs::symlink(&link_target, &link_path).expect("create symlink");

    let ctx = test_ctx(home, output.clone());
    let symlink_entry = Entry {
        absolute_path: link_path,
        relative_path: PathBuf::from(".config/mylink"),
        root: EntryRoot::Home,
        entry_type: EntryType::Symlink,
        category: catalog::SHELL.name,
        sensitive: false,
    };
    let selection = SelectionModel::new(vec![symlink_entry]);

    let outcome = run_backup(&selection, &ctx, &Encryption::None).expect("backup should succeed");

    // The Manifest records the Entry's `type` as symlink (story 43) and nothing Vanished...
    assert_eq!(outcome.manifest.entries.len(), 1);
    let manifest_entry = &outcome.manifest.entries[0];
    assert_eq!(manifest_entry.path, ".config/mylink");
    assert_eq!(
        manifest_entry.entry_type,
        manifest::ManifestEntryType::Symlink,
        "the Manifest must record the Entry's type as symlink"
    );
    assert!(outcome.manifest.vanished.is_empty());
    // ...and a symlink carries no per-file checksum (it has no file bytes; its target lives
    // in the tar stream).
    assert!(
        !outcome
            .manifest
            .checksums
            .contains_key("home/.config/mylink"),
        "a symlink Entry must not record a file-content checksum: {:?}",
        outcome.manifest.checksums
    );

    // Restore into a DIFFERENT fresh root.
    let restore_home = tmp.path().join("restored-home");
    let restore_ctx = test_ctx(restore_home.clone(), output);
    let restore_outcome =
        run_restore(&outcome.backup_dir, &restore_ctx, None).expect("restore should succeed");
    assert!(
        restore_outcome.failures.is_empty(),
        "unexpected restore failures: {:?}",
        restore_outcome.failures
    );

    // The restored path is a symlink (NOT a regular file of the target's bytes), pointing at
    // the original target path verbatim.
    let restored_link = restore_home.join(".config/mylink");
    let restored_meta =
        fs::symlink_metadata(&restored_link).expect("restored symlink should exist");
    assert!(
        restored_meta.file_type().is_symlink(),
        "the restored Entry must be a symlink, not a dereferenced copy of the target"
    );
    assert_eq!(
        fs::read_link(&restored_link).expect("restored symlink should be readable"),
        link_target,
        "the restored symlink must point at the original target path"
    );
}
