//! `core`: linux-backup's UI-free domain library.
//!
//! Owns the Selection model and all backup/restore logic. Per ADR-0001, this crate's
//! Cargo.toml must never depend on `inquire`, `ratatui`, or any other terminal/rendering
//! crate — the core/tui split is compiler-enforced, not just a convention.
//!
//! Per ADR-0004, `core` reads nothing from the ambient system (no `std::env`, `dirs`,
//! `hostname`, `getuid`, or clock calls anywhere in this crate). Every run fact it needs
//! arrives through an injected [`context::RunContext`], filled in by `crates/tui`'s `main`
//! from the real machine, or by tests with fixed values.

pub mod archive;
pub mod catalog;
pub mod context;
pub mod crypto;
pub mod error;
pub mod location;
pub mod manifest;
pub mod model;
pub mod profile;
pub mod progress;
pub mod restore;
pub mod scan;

pub use context::{Privilege, RunContext};
pub use error::CoreError;
pub use model::SelectionModel;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use archive::Encryption;
use manifest::Manifest;

/// The result of a completed backup run: where the Backup dir landed and the Manifest
/// that was written into it.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    pub backup_dir: PathBuf,
    pub manifest: Manifest,
}

/// `core`'s public backup entry point — the seam the round-trip test
/// (`crates/core/tests/roundtrip.rs`) drives, per issue #17's Testing Decisions.
///
/// Computes the Backup dir name (`linux-backup-<host>-<timestamp>`, from `ctx` alone —
/// never the ambient clock/hostname, per ADR-0004) under `ctx.output_location`, and
/// refuses if it already exists (story 33: the Backup dir is the collision unit). Builds
/// the Archive and Manifest from `selection`'s Entries, applying `encryption` to both (all
/// -or-nothing, per CONTEXT.md's Manifest entry).
///
/// Per ADR-0002, backup fails fast: any error after the Backup dir is created deletes it
/// again before returning `Err`, so a Backup dir with a Manifest in it is always a
/// complete, trustworthy one (story 41).
pub fn backup(
    selection: &SelectionModel,
    ctx: &RunContext,
    encryption: &Encryption,
) -> Result<BackupOutcome, CoreError> {
    let backup_dir = ctx.output_location.join(backup_dir_name(ctx));

    if backup_dir.exists() {
        return Err(CoreError::BackupDirExists(backup_dir));
    }

    run_backup(selection, ctx, encryption, &backup_dir).inspect_err(|_| {
        // Fail-fast cleanup (ADR-0002): the Backup dir was created fresh by this call, so
        // removing it on any error is always safe.
        let _ = fs::remove_dir_all(&backup_dir);
    })
}

fn run_backup(
    selection: &SelectionModel,
    ctx: &RunContext,
    encryption: &Encryption,
    backup_dir: &Path,
) -> Result<BackupOutcome, CoreError> {
    fs::create_dir_all(backup_dir).map_err(|source| CoreError::Io {
        path: backup_dir.to_path_buf(),
        source,
    })?;

    let built = archive::build(selection.entries())?;

    let archive_sha256 = archive::sha256_hex(&built.plaintext_bytes);
    let archive_bytes_on_disk = archive::encrypt(&built.plaintext_bytes, encryption)?;
    let archive_path = backup_dir.join(archive::archive_filename(encryption));
    archive::write_bytes(&archive_path, &archive_bytes_on_disk)?;

    let vanished = built
        .vanished
        .into_iter()
        .map(|v| manifest::VanishedEntry {
            path: v.archive_path,
            sensitive: v.sensitive,
        })
        .collect();

    let manifest = manifest::build(manifest::ManifestInputs {
        ctx,
        tool_version: env!("CARGO_PKG_VERSION"),
        entries: selection.entries(),
        checksums: built.checksums,
        archive_sha256,
        vanished,
    });

    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    let manifest_bytes_on_disk = archive::encrypt(&manifest_json, encryption)?;
    let manifest_path = backup_dir.join(archive::manifest_filename(encryption));
    archive::write_bytes(&manifest_path, &manifest_bytes_on_disk)?;

    Ok(BackupOutcome {
        backup_dir: backup_dir.to_path_buf(),
        manifest,
    })
}

/// `linux-backup-<host>-<timestamp>`, built entirely from `ctx` (ADR-0004) so it's
/// deterministic under a fixed `RunContext` in tests.
fn backup_dir_name(ctx: &RunContext) -> String {
    let timestamp = ctx.now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("linux-backup-{}-{}", ctx.host, timestamp)
}
