//! Untar-to-path restore with privilege-aware uid/gid handling and Re-rooting
//! (home-tagged Entries re-based under the restoring user's `$HOME`).
//!
//! Per ADR-0002, restore is best-effort: it continues past per-file failures and collects
//! a `path + reason` report, aborting only on a pre-flight `archive_sha256` mismatch.
//!
//! #19 scope: the whole-archive restore + Re-rooting shape from the round-trip seam.
//! Privilege-aware uid/gid restore (story 53), the overwrite confirmation (story 54), and
//! `reinstall.sh` generation (story 56) are later sub-issues' jobs (#24/#26).

use std::fs;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::context::RunContext;
use crate::error::CoreError;
use crate::manifest::Manifest;

/// One per-Entry failure encountered during restore. Per ADR-0002, restore continues past
/// these rather than aborting the whole run — collected here instead.
#[derive(Debug, Clone)]
pub struct RestoreFailure {
    pub path: String,
    pub reason: String,
}

/// The result of a completed restore run.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// The Manifest read back from the Backup dir.
    pub manifest: Manifest,
    /// Every target path a file was actually written to.
    pub restored_paths: Vec<PathBuf>,
    /// Per-file failures that didn't abort the run (ADR-0002).
    pub failures: Vec<RestoreFailure>,
}

/// Reads the Backup dir at `backup_dir`: locates the Manifest and Archive (encrypted or
/// not, by filename suffix), decrypts (a no-op stub for #19 — see `archive`), verifies the
/// Archive's checksum against `manifest.archive_sha256` (aborting before any file is
/// touched on mismatch, per ADR-0002), then untars every Entry into `ctx` — Re-rooting
/// `home`-tagged Entries under `ctx.home`, the *restoring* context's home, regardless of
/// whatever `home` the Manifest itself recorded.
pub fn restore(backup_dir: &Path, ctx: &RunContext) -> Result<RestoreOutcome, CoreError> {
    let (manifest_path, encrypted) = locate_manifest(backup_dir)?;
    let manifest_bytes = fs::read(&manifest_path).map_err(|source| CoreError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest_bytes = archive::decrypt(&manifest_bytes, encrypted)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;

    let archive_path = backup_dir.join(archive::filename_for(archive::ARCHIVE_FILENAME, encrypted));
    let archive_bytes = fs::read(&archive_path).map_err(|source| CoreError::Io {
        path: archive_path.clone(),
        source,
    })?;
    let archive_bytes = archive::decrypt(&archive_bytes, encrypted)?;

    let actual_sha256 = archive::sha256_hex(&archive_bytes);
    if actual_sha256 != manifest.archive_sha256 {
        return Err(CoreError::ArchiveChecksumMismatch {
            expected: manifest.archive_sha256.clone(),
            actual: actual_sha256,
        });
    }

    let (restored_paths, failures) = unpack_all(&archive_bytes, ctx)?;

    Ok(RestoreOutcome {
        manifest,
        restored_paths,
        failures,
    })
}

/// Locates the Manifest in `backup_dir`, returning its path and whether the Backup is
/// encrypted (the `.age` filename suffix is the authoritative signal, per CONTEXT.md's
/// Manifest entry — never a field inside the Manifest itself).
fn locate_manifest(backup_dir: &Path) -> Result<(PathBuf, bool), CoreError> {
    let encrypted_path = backup_dir.join(archive::filename_for(archive::MANIFEST_FILENAME, true));
    if encrypted_path.exists() {
        return Ok((encrypted_path, true));
    }
    let plain_path = backup_dir.join(archive::filename_for(archive::MANIFEST_FILENAME, false));
    if plain_path.exists() {
        return Ok((plain_path, false));
    }
    Err(CoreError::ManifestMissing(backup_dir.to_path_buf()))
}

/// Untars every entry in the decrypted, decompressed-on-the-fly Archive bytes, Re-rooting
/// as it goes. Per ADR-0002, a per-file failure is collected and iteration continues.
fn unpack_all(archive_bytes: &[u8], ctx: &RunContext) -> Result<(Vec<PathBuf>, Vec<RestoreFailure>), CoreError> {
    let zstd_decoder =
        zstd::stream::read::Decoder::new(archive_bytes).map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    let mut tar_archive = tar::Archive::new(zstd_decoder);
    let tar_entries = tar_archive
        .entries()
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;

    let mut restored_paths = Vec::new();
    let mut failures = Vec::new();

    for tar_entry in tar_entries {
        let mut tar_entry = match tar_entry {
            Ok(entry) => entry,
            Err(e) => {
                failures.push(RestoreFailure {
                    path: "<unreadable tar entry>".to_string(),
                    reason: e.to_string(),
                });
                continue;
            }
        };

        let entry_path_in_tar = match tar_entry.path() {
            Ok(p) => p.into_owned(),
            Err(e) => {
                failures.push(RestoreFailure {
                    path: "<unreadable tar entry path>".to_string(),
                    reason: e.to_string(),
                });
                continue;
            }
        };

        let target = match resolve_target(&entry_path_in_tar, ctx) {
            Ok(target) => target,
            Err(reason) => {
                failures.push(RestoreFailure {
                    path: entry_path_in_tar.display().to_string(),
                    reason,
                });
                continue;
            }
        };

        match unpack_one(&mut tar_entry, &target) {
            Ok(()) => restored_paths.push(target),
            Err(source) => failures.push(RestoreFailure {
                path: target.display().to_string(),
                reason: source.to_string(),
            }),
        }
    }

    Ok((restored_paths, failures))
}

/// Resolves a tar-internal path (`home/...` or `absolute/...`) to a real target path.
/// Re-rooting: a `home/...` entry lands under `ctx.home` (the *restoring* context's home),
/// never the Manifest's original `home` — see CONTEXT.md's "Re-rooting".
fn resolve_target(entry_path_in_tar: &Path, ctx: &RunContext) -> Result<PathBuf, String> {
    let mut components = entry_path_in_tar.components();
    let root_tag = components.next().ok_or_else(|| "empty tar path".to_string())?;
    let rest: PathBuf = components.collect();

    match root_tag.as_os_str().to_str() {
        Some("home") => Ok(ctx.home.join(rest)),
        Some("absolute") => Ok(Path::new("/").join(rest)),
        other => Err(format!("unrecognized Archive root tag: {other:?}")),
    }
}

fn unpack_one<R: std::io::Read>(tar_entry: &mut tar::Entry<'_, R>, target: &Path) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    tar_entry.unpack(target)?;
    Ok(())
}
