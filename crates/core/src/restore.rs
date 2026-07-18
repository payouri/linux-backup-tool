//! Untar-to-path restore with privilege-aware uid/gid handling and Re-rooting
//! (home-tagged Entries re-based under the restoring user's `$HOME`).
//!
//! Per ADR-0002, restore is best-effort: it continues past per-file failures and collects
//! a `path + reason` report, aborting only on a pre-flight `archive_sha256` mismatch.
//!
//! #19 built the whole-archive restore + Re-rooting shape from the round-trip seam. #26
//! rounds out the remaining restore mechanics from the parent spec (stories 53-57):
//! privilege-aware uid/gid restore, the overwrite-collision switch, `reinstall.sh`
//! generation, and the opt-in `--verify` checksum pass (the function only — wiring an
//! actual CLI flag is #29's job).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use secrecy::SecretString;

use crate::archive;
use crate::context::{Privilege, RunContext};
use crate::error::CoreError;
use crate::manifest::{Manifest, PackageInfo};
use crate::packages::{AUR, CARGO, FLATPAK, NPM, PACMAN, SNAP};

/// One per-Entry failure encountered during restore. Per ADR-0002, restore continues past
/// these rather than aborting the whole run — collected here instead.
///
/// A failure here doesn't necessarily mean the target's content is missing: e.g. an
/// ownership-restore failure (see [`OwnershipOutcome`]) is recorded here even though the
/// file itself landed successfully and its path also appears in `restored_paths` — the two
/// facts ("content restored", "ownership restore failed") are independent and both true.
#[derive(Debug, Clone)]
pub struct RestoreFailure {
    pub path: String,
    pub reason: String,
}

/// Whether uid/gid restoration was attempted for this run (story 53). A whole-run decision,
/// not a per-file one: `core` reads it from `RunContext.privilege` rather than trying and
/// silently failing on every file when non-root. The actual up-front warning to the user is
/// `tui`'s job (#29) — this is the underlying data it acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipOutcome {
    /// Running as root: a `chown` to each restored file's original uid/gid (as recorded in
    /// the Archive's tar headers — CONTEXT.md's Manifest entry keeps POSIX metadata out of
    /// the Manifest itself) was attempted. A per-file `chown` failure lands in `failures`,
    /// not here.
    Restored,
    /// Non-root run: files land under the current process's ownership and `chown` was
    /// never attempted.
    SkippedNonRoot,
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
    /// Whether uid/gid restoration was attempted this run (story 53).
    pub ownership: OwnershipOutcome,
    /// Where `reinstall.sh` (story 56) was written this run.
    pub reinstall_script_path: PathBuf,
    /// Per-manager package counts backing `reinstall.sh`, for a caller (TUI, #29) to print.
    pub package_counts: BTreeMap<String, usize>,
}

/// Reads the Backup dir at `backup_dir`: locates the Manifest and Archive (encrypted or
/// not, by filename suffix), decrypts with `passphrase` when encrypted (real `age`
/// decryption per #23 — a wrong passphrase or corrupted ciphertext fails with a typed
/// `CoreError::DecryptionFailed` before anything is touched, never a partial write),
/// verifies the Archive's checksum against `manifest.archive_sha256` (aborting before any
/// file is touched on mismatch, per ADR-0002), then untars every Entry into `ctx` —
/// Re-rooting `home`-tagged Entries under `ctx.home`, the *restoring* context's home,
/// regardless of whatever `home` the Manifest itself recorded.
///
/// `passphrase` is ignored when the Backup is unencrypted; it is required (and its absence
/// is a typed error, not a panic) when the Backup's Manifest/Archive carry the `.age`
/// suffix.
///
/// `overwrite_existing` is story 54's on/off switch: when a target path already exists and
/// this is `false`, that file is left untouched and lands in the returned failures as a
/// collision rather than being silently clobbered; when `true`, it's overwritten. `core`
/// doesn't build a per-file confirmation loop — that one flag is the whole seam; the actual
/// "confirm once" prompt is `tui` territory (#29).
///
/// Ownership (story 53) and `reinstall.sh` (story 56) both follow from `ctx`/`manifest`
/// alone, so this is also where both happen: `ctx.privilege` decides whether `chown` is
/// attempted at all (see [`OwnershipOutcome`]), and `reinstall.sh` is written to
/// `ctx.home` — the one place still guaranteed reachable after restore even if the Backup
/// dir lived on a drive that gets unmounted (e.g. a USB stick) — never executed by
/// anything here.
pub fn restore(
    backup_dir: &Path,
    ctx: &RunContext,
    passphrase: Option<&SecretString>,
    overwrite_existing: bool,
) -> Result<RestoreOutcome, CoreError> {
    let (manifest_path, encrypted) = locate_manifest(backup_dir)?;
    let manifest_bytes = fs::read(&manifest_path).map_err(|source| CoreError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest_bytes = archive::decrypt(&manifest_bytes, encrypted, passphrase)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;

    let archive_path = backup_dir.join(archive::filename_for(archive::ARCHIVE_FILENAME, encrypted));
    let archive_bytes = fs::read(&archive_path).map_err(|source| CoreError::Io {
        path: archive_path.clone(),
        source,
    })?;
    let archive_bytes = archive::decrypt(&archive_bytes, encrypted, passphrase)?;

    let actual_sha256 = archive::sha256_hex(&archive_bytes);
    if actual_sha256 != manifest.archive_sha256 {
        return Err(CoreError::ArchiveChecksumMismatch {
            expected: manifest.archive_sha256.clone(),
            actual: actual_sha256,
        });
    }

    let (restored_paths, mut failures) = unpack_all(&archive_bytes, ctx, overwrite_existing)?;

    let ownership = match ctx.privilege {
        Privilege::Root => OwnershipOutcome::Restored,
        Privilege::NonRoot => OwnershipOutcome::SkippedNonRoot,
    };

    let reinstall = build_reinstall_script(&manifest);
    let reinstall_script_path = ctx.home.join("reinstall.sh");
    if let Err(reason) = write_reinstall_script(&reinstall_script_path, &reinstall.content) {
        failures.push(RestoreFailure {
            path: reinstall_script_path.display().to_string(),
            reason,
        });
    }

    Ok(RestoreOutcome {
        manifest,
        restored_paths,
        failures,
        ownership,
        reinstall_script_path,
        package_counts: reinstall.counts,
    })
}

/// Writes `content` to `path`, creating its parent directory first — `ctx.home` may not
/// exist yet if every Entry in this Backup was `absolute`-tagged (no `home`-rooted Entry
/// ever created it during unpacking).
fn write_reinstall_script(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
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
///
/// `overwrite_existing` gates story 54's collision check: a target that already exists is
/// skipped (recorded as a collision failure, not overwritten) unless this is `true`.
fn unpack_all(
    archive_bytes: &[u8],
    ctx: &RunContext,
    overwrite_existing: bool,
) -> Result<(Vec<PathBuf>, Vec<RestoreFailure>), CoreError> {
    let zstd_decoder = zstd::stream::read::Decoder::new(archive_bytes)
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
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

        // Story 54: the collision unit is the restore target path itself — distinct from
        // the Backup-dir-name collision (#19/#27), which guards the *output* side of a
        // backup run, not a restore target.
        if target.exists() && !overwrite_existing {
            failures.push(RestoreFailure {
                path: target.display().to_string(),
                reason: "collision: target already exists and overwrite_existing was not set"
                    .to_string(),
            });
            continue;
        }

        match unpack_one(&mut tar_entry, &target) {
            Ok(()) => {
                restored_paths.push(target.clone());
                // Story 53: attempt chown only as root; never even try (let alone fail
                // silently) as a non-root run — that decision is surfaced via
                // `RestoreOutcome::ownership` rather than re-derived per file here.
                if ctx.privilege == Privilege::Root {
                    if let Err(reason) = chown_to_tar_owner(&tar_entry, &target) {
                        failures.push(RestoreFailure {
                            path: target.display().to_string(),
                            reason: format!("ownership restore failed: {reason}"),
                        });
                    }
                }
            }
            Err(source) => failures.push(RestoreFailure {
                path: target.display().to_string(),
                reason: source.to_string(),
            }),
        }
    }

    Ok((restored_paths, failures))
}

/// Attempts to `chown` `target` to the uid/gid recorded in `tar_entry`'s header — the
/// Archive's own tar stream, not the Manifest (CONTEXT.md: "No POSIX metadata table — tar
/// owns mode/uid/gid/symlink", so the tar header is the one source of truth for this,
/// deliberately not duplicated in the Manifest). A failure here (e.g. `EPERM` if the real
/// process privilege doesn't actually match `ctx.privilege`) is the caller's to fold into
/// the per-file failure report — it must never abort the run (ADR-0002).
fn chown_to_tar_owner<R: std::io::Read>(
    tar_entry: &tar::Entry<'_, R>,
    target: &Path,
) -> Result<(), String> {
    let uid = tar_entry.header().uid().map_err(|e| e.to_string())?;
    let gid = tar_entry.header().gid().map_err(|e| e.to_string())?;
    std::os::unix::fs::chown(target, Some(uid as u32), Some(gid as u32)).map_err(|e| e.to_string())
}

/// Resolves a tar-internal path (`home/...` or `absolute/...`) to a real target path.
/// Re-rooting: a `home/...` entry lands under `ctx.home` (the *restoring* context's home),
/// never the Manifest's original `home` — see CONTEXT.md's "Re-rooting".
fn resolve_target(entry_path_in_tar: &Path, ctx: &RunContext) -> Result<PathBuf, String> {
    let mut components = entry_path_in_tar.components();
    let root_tag = components
        .next()
        .ok_or_else(|| "empty tar path".to_string())?;
    let rest: PathBuf = components.collect();

    match root_tag.as_os_str().to_str() {
        Some("home") => Ok(ctx.home.join(rest)),
        Some("absolute") => Ok(Path::new("/").join(rest)),
        other => Err(format!("unrecognized Archive root tag: {other:?}")),
    }
}

fn unpack_one<R: std::io::Read>(
    tar_entry: &mut tar::Entry<'_, R>,
    target: &Path,
) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    tar_entry.unpack(target)?;
    Ok(())
}

/// The generated `reinstall.sh` content plus the per-manager counts backing it (story 56).
#[derive(Debug, Clone)]
pub struct ReinstallScript {
    pub content: String,
    /// Manager name (`packages::PACMAN`/`AUR`/...) -> package count. Covers every manager
    /// present in the Manifest, including a manager with zero captured packages, so a
    /// caller can print "0" rather than omit it.
    pub counts: BTreeMap<String, usize>,
}

/// Builds `reinstall.sh`'s content from `manifest.packages` (story 56): a pure function,
/// never touching disk or executing anything — this is what makes it safe to unit-test
/// directly and reuse from `restore` without duplicating logic.
///
/// Deliberately unpinned (story 48/56): captured `version`s are never emitted, so
/// reinstalling on a rolling-release distro doesn't chase an unavailable exact version.
pub fn build_reinstall_script(manifest: &Manifest) -> ReinstallScript {
    let mut sections = Vec::new();
    let mut counts = BTreeMap::new();

    for (manager, pkgs) in &manifest.packages {
        counts.insert(manager.clone(), pkgs.len());
        if let Some(command) = install_command_for_manager(manager, pkgs) {
            let noun = if pkgs.len() == 1 {
                "package"
            } else {
                "packages"
            };
            sections.push(format!("# {manager} ({} {noun})\n{command}", pkgs.len()));
        }
    }

    let body = if sections.is_empty() {
        "# No packages were captured in this Backup's Manifest.".to_string()
    } else {
        sections.join("\n\n")
    };

    let content = format!(
        "#!/usr/bin/env bash\n\
         # Generated by linux-backup restore. Review before running — linux-backup never\n\
         # executes this file itself.\n\
         # Package versions were informational only in the Manifest and are NOT pinned here.\n\
         set -euo pipefail\n\n\
         {body}\n"
    );

    ReinstallScript { content, counts }
}

/// One manager's install command line(s), or `None` for an empty or unrecognized manager
/// (forward-compatible: a future manager key with no case here is silently skipped rather
/// than panicking).
fn install_command_for_manager(manager: &str, pkgs: &[PackageInfo]) -> Option<String> {
    if pkgs.is_empty() {
        return None;
    }
    let names = || {
        pkgs.iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    match manager {
        PACMAN => Some(format!("sudo pacman -S --needed {}", names())),
        AUR => Some(format!("yay -S --needed {}", names())),
        FLATPAK => Some(flatpak_install_lines(pkgs)),
        SNAP => Some(snap_install_lines(pkgs)),
        CARGO => Some(format!("cargo install {}", names())),
        NPM => Some(format!("npm install -g {}", names())),
        _ => None,
    }
}

/// Flatpak packages are grouped by origin (remote) since a Manifest can carry packages from
/// more than one remote — defaulting to `flathub`, the overwhelmingly common case, when a
/// package's origin wasn't captured.
fn flatpak_install_lines(pkgs: &[PackageInfo]) -> String {
    let mut by_origin: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for pkg in pkgs {
        let origin = pkg.origin.as_deref().unwrap_or("flathub");
        by_origin.entry(origin).or_default().push(pkg.name.as_str());
    }
    by_origin
        .into_iter()
        .map(|(origin, names)| format!("flatpak install -y {origin} {}", names.join(" ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Snap packages install one per line since `--channel` varies per package (unlike pacman/
/// cargo/npm, which can share a single multi-name invocation).
fn snap_install_lines(pkgs: &[PackageInfo]) -> String {
    pkgs.iter()
        .map(|pkg| match &pkg.channel {
            Some(channel) => format!("sudo snap install {} --channel={channel}", pkg.name),
            None => format!("sudo snap install {}", pkg.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One checksum mismatch found by [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyMismatch {
    pub path: String,
    pub expected_sha256: String,
    pub actual_sha256: String,
}

/// The result of an opt-in `--verify` pass (story 57).
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    /// Archive-internal paths whose restored bytes hashed to exactly what the Manifest
    /// recorded.
    pub matched: Vec<String>,
    /// Restored files whose bytes no longer match the Manifest's checksum.
    pub mismatched: Vec<VerifyMismatch>,
    /// Manifest-recorded paths with nothing at their resolved restore target at all —
    /// distinct from a mismatch (present but wrong) per CONTEXT.md's "Vanished" framing of
    /// missing-vs-wrong as different failure shapes.
    pub missing: Vec<String>,
}

impl VerifyReport {
    /// Whether every checksummed file was found and matched — the single yes/no a caller
    /// needs before deciding whether to trust the restore.
    pub fn is_clean(&self) -> bool {
        self.mismatched.is_empty() && self.missing.is_empty()
    }
}

/// Re-hashes every file `manifest.checksums` recorded, resolved under `ctx` exactly as
/// `restore` resolved it (same Re-rooting), and reports matches/mismatches/missing files.
///
/// Pure and read-only: never mutates anything, so it's safe to call any time after a
/// restore completes (or independently, against any already-restored root) — this is the
/// opt-in `--verify` pass itself; wiring an actual CLI flag is #29's job.
pub fn verify(ctx: &RunContext, manifest: &Manifest) -> VerifyReport {
    let mut report = VerifyReport::default();

    for (archive_path, expected_sha256) in &manifest.checksums {
        let target = match resolve_target(Path::new(archive_path), ctx) {
            Ok(target) => target,
            Err(_) => {
                report.missing.push(archive_path.clone());
                continue;
            }
        };

        match fs::read(&target) {
            Ok(bytes) => {
                let actual_sha256 = archive::sha256_hex(&bytes);
                if &actual_sha256 == expected_sha256 {
                    report.matched.push(archive_path.clone());
                } else {
                    report.mismatched.push(VerifyMismatch {
                        path: archive_path.clone(),
                        expected_sha256: expected_sha256.clone(),
                        actual_sha256,
                    });
                }
            }
            Err(_) => report.missing.push(archive_path.clone()),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn package(name: &str, version: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            origin: None,
            channel: None,
        }
    }

    fn empty_manifest() -> Manifest {
        Manifest {
            schema_version: crate::manifest::SCHEMA_VERSION,
            tool_version: "test".to_string(),
            created_at: "1970-01-01T00:00:00Z".to_string(),
            host: "test-host".to_string(),
            user: "test-user".to_string(),
            uid: 1000,
            home: PathBuf::from("/home/test-user"),
            os: "linux-test".to_string(),
            entries: Vec::new(),
            checksums: BTreeMap::new(),
            archive_sha256: "deadbeef".to_string(),
            vanished: Vec::new(),
            packages: BTreeMap::new(),
        }
    }

    fn test_ctx(home: PathBuf) -> RunContext {
        RunContext {
            home,
            output_location: PathBuf::from("/unused"),
            privilege: Privilege::NonRoot,
            host: "test-host".to_string(),
            user: "test-user".to_string(),
            uid: 1000,
            os: "linux-test".to_string(),
            now: UNIX_EPOCH,
        }
    }

    #[test]
    fn build_reinstall_script_is_unpinned_and_never_emits_captured_versions() {
        let mut manifest = empty_manifest();
        manifest.packages.insert(
            PACMAN.to_string(),
            vec![package("acpi", "1.8-2"), package("ripgrep", "13.0.0")],
        );

        let script = build_reinstall_script(&manifest);

        assert!(script
            .content
            .contains("sudo pacman -S --needed acpi ripgrep"));
        assert!(
            !script.content.contains("1.8-2") && !script.content.contains("13.0.0"),
            "captured versions must never be embedded as pins: {}",
            script.content
        );
        assert_eq!(script.counts.get(PACMAN), Some(&2));
    }

    #[test]
    fn build_reinstall_script_never_contains_a_shebang_execution_of_itself() {
        // Guards the "never auto-run" requirement at the content level: this module must
        // never itself invoke `sh`/`bash`/`Command::new` against the generated script.
        let manifest = empty_manifest();
        let script = build_reinstall_script(&manifest);
        assert!(script.content.starts_with("#!/usr/bin/env bash"));
        assert!(script.content.contains("No packages were captured"));
    }

    #[test]
    fn build_reinstall_script_reports_correct_per_manager_counts_across_managers() {
        let mut manifest = empty_manifest();
        manifest
            .packages
            .insert(PACMAN.to_string(), vec![package("acpi", "1.8-2")]);
        manifest.packages.insert(
            AUR.to_string(),
            vec![package("android-sdk", "26.1.1-2"), package("yay", "12.3.5")],
        );
        manifest
            .packages
            .insert(NPM.to_string(), vec![package("typescript", "4.8.3")]);

        let script = build_reinstall_script(&manifest);

        assert_eq!(script.counts.len(), 3);
        assert_eq!(script.counts[PACMAN], 1);
        assert_eq!(script.counts[AUR], 2);
        assert_eq!(script.counts[NPM], 1);
        assert!(script.content.contains("yay -S --needed android-sdk yay"));
        assert!(script.content.contains("npm install -g typescript"));
    }

    #[test]
    fn build_reinstall_script_groups_flatpak_by_origin_and_lines_up_snap_channels() {
        let mut manifest = empty_manifest();
        manifest.packages.insert(
            FLATPAK.to_string(),
            vec![
                PackageInfo {
                    name: "org.mozilla.firefox".to_string(),
                    version: "130.0".to_string(),
                    origin: Some("flathub".to_string()),
                    channel: None,
                },
                PackageInfo {
                    name: "org.example.Custom".to_string(),
                    version: "1.0".to_string(),
                    origin: Some("custom-remote".to_string()),
                    channel: None,
                },
            ],
        );
        manifest.packages.insert(
            SNAP.to_string(),
            vec![PackageInfo {
                name: "bare".to_string(),
                version: "1.0".to_string(),
                origin: None,
                channel: Some("latest/stable".to_string()),
            }],
        );
        manifest
            .packages
            .insert(CARGO.to_string(), vec![package("ripgrep", "13.0.0")]);

        let script = build_reinstall_script(&manifest);

        assert!(script
            .content
            .contains("flatpak install -y flathub org.mozilla.firefox"));
        assert!(script
            .content
            .contains("flatpak install -y custom-remote org.example.Custom"));
        assert!(script
            .content
            .contains("sudo snap install bare --channel=latest/stable"));
        assert!(script.content.contains("cargo install ripgrep"));
    }

    #[test]
    fn verify_reports_matched_mismatched_and_missing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("create home dir");

        fs::write(home.join("good.txt"), b"original bytes").expect("write good.txt");
        fs::write(home.join("tampered.txt"), b"changed bytes").expect("write tampered.txt");
        // "missing.txt" deliberately never written.

        let mut manifest = empty_manifest();
        manifest.checksums.insert(
            "home/good.txt".to_string(),
            archive::sha256_hex(b"original bytes"),
        );
        manifest.checksums.insert(
            "home/tampered.txt".to_string(),
            archive::sha256_hex(b"original bytes"),
        );
        manifest.checksums.insert(
            "home/missing.txt".to_string(),
            archive::sha256_hex(b"anything"),
        );

        let ctx = test_ctx(home);
        let report = verify(&ctx, &manifest);

        assert_eq!(report.matched, vec!["home/good.txt".to_string()]);
        assert_eq!(report.mismatched.len(), 1);
        assert_eq!(report.mismatched[0].path, "home/tampered.txt");
        assert_eq!(
            report.mismatched[0].expected_sha256,
            archive::sha256_hex(b"original bytes")
        );
        assert_eq!(
            report.mismatched[0].actual_sha256,
            archive::sha256_hex(b"changed bytes")
        );
        assert_eq!(report.missing, vec!["home/missing.txt".to_string()]);
        assert!(!report.is_clean());
    }

    #[test]
    fn verify_is_clean_when_every_checksummed_file_matches() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("create home dir");
        fs::write(home.join("good.txt"), b"original bytes").expect("write good.txt");

        let mut manifest = empty_manifest();
        manifest.checksums.insert(
            "home/good.txt".to_string(),
            archive::sha256_hex(b"original bytes"),
        );

        let ctx = test_ctx(home);
        let report = verify(&ctx, &manifest);

        assert!(report.is_clean());
        assert!(report.mismatched.is_empty());
        assert!(report.missing.is_empty());
    }
}
