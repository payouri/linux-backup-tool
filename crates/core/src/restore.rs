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
use crate::progress::{Phase, Progress};

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

/// A restore target that already existed and was deliberately left untouched because the
/// [`CollisionPolicy`] declined to overwrite it (a `--overwrite`-less run whose confirmation
/// was declined). Kept structurally separate from [`RestoreFailure`] per issue #37: a
/// user-declined collision is a deliberate skip, not an error, and a caller (and the run
/// summary it prints) must be able to tell the two apart without parsing reason strings.
#[derive(Debug, Clone)]
pub struct SkippedCollision {
    pub path: String,
}

/// How `restore` resolves a target-path collision — an existing file where an Entry would
/// otherwise land. Story 54 / issue #37.
///
/// Because the confirmation must be a *single* "confirm once" decision covering every
/// collision at once (never one prompt per file), `core` pre-scans the whole Archive for
/// collisions up front and, only if there is at least one, hands the full list to the
/// decision. `core` never prompts: per ADR-0001 the `inquire` confirmation lives in `tui`,
/// which plugs into the [`Confirm`](CollisionPolicy::Confirm) callback. The decision crosses
/// the core/tui boundary as data (a list of colliding paths in, a yes/no out), so `core`
/// stays UI-free.
pub enum CollisionPolicy<'a> {
    /// `--overwrite`: every colliding target is overwritten without asking — unchanged
    /// non-interactive AFK behaviour. `core` does not even pre-scan for collisions in this
    /// case (there is nothing to ask about).
    Overwrite,
    /// Ask once. `core` collects every colliding target and, if the list is non-empty, calls
    /// this callback exactly once with the full list; returning `true` overwrites them all,
    /// `false` skips them all (each recorded in [`RestoreOutcome::skipped_collisions`], not
    /// [`RestoreOutcome::failures`]). This is the seam `tui`'s single `inquire` confirmation
    /// prompt plugs into.
    Confirm(&'a mut dyn FnMut(&[PathBuf]) -> bool),
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
    /// Targets left untouched because the [`CollisionPolicy`] declined to overwrite them —
    /// deliberate skips, distinct from [`failures`](RestoreOutcome::failures) (issue #37).
    pub skipped_collisions: Vec<SkippedCollision>,
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
/// `policy` (story 54 / issue #37) decides what happens at a collision — a target path that
/// already exists where an Entry would land. [`CollisionPolicy::Overwrite`] clobbers every
/// collision without asking (the `--overwrite` AFK path); [`CollisionPolicy::Confirm`] makes
/// `core` pre-scan every collision and, only if there is at least one, hand the full list to
/// the injected callback for a single yes/no. A declined collision leaves the target
/// untouched and lands in [`RestoreOutcome::skipped_collisions`] — distinct from a real error
/// in [`RestoreOutcome::failures`]. `core` never prompts: the `inquire` confirmation lives in
/// `tui` (ADR-0001), behind that callback.
///
/// Ownership (story 53) and `reinstall.sh` (story 56) both follow from `ctx`/`manifest`
/// alone, so this is also where both happen: `ctx.privilege` decides whether `chown` is
/// attempted at all (see [`OwnershipOutcome`]), and `reinstall.sh` is written to
/// `ctx.home` — the one place still guaranteed reachable after restore even if the Backup
/// dir lived on a drive that gets unmounted (e.g. a USB stick) — never executed by
/// anything here.
///
/// `progress` is the injected callback (ADR-0004: an injected notification, not an ambient
/// read) the restore screen drives its progress from, mirroring how [`backup`](crate::backup)
/// takes one. It observes [`Phase::VerifyingArchive`] (the pre-flight checksum pass) then
/// [`Phase::Restoring`] with a per-Entry tick as files land. Like backup's, it is only ever
/// notified, never consulted; callers that don't care pass `&mut progress::NoopProgress`.
pub fn restore(
    backup_dir: &Path,
    ctx: &RunContext,
    passphrase: Option<&SecretString>,
    mut policy: CollisionPolicy<'_>,
    progress: &mut dyn Progress,
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

    progress.on_phase(Phase::VerifyingArchive);
    let actual_sha256 = archive::sha256_hex(&archive_bytes);
    if actual_sha256 != manifest.archive_sha256 {
        return Err(CoreError::ArchiveChecksumMismatch {
            expected: manifest.archive_sha256,
            actual: actual_sha256,
        });
    }

    // Pre-scan every collision and resolve the whole-run overwrite decision *before* any
    // write, so the confirmation (when interactive) can name every colliding target in one
    // "confirm once" prompt (issue #37) rather than pausing per file.
    let overwrite = resolve_overwrite(&archive_bytes, ctx, &mut policy)?;

    progress.on_phase(Phase::Restoring);
    let UnpackReport {
        restored_paths,
        mut failures,
        skipped_collisions,
    } = unpack_all(
        &archive_bytes,
        ctx,
        overwrite,
        manifest.entries.len(),
        progress,
    )?;

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
        skipped_collisions,
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

/// What [`unpack_all`] observed: files that landed, failures that didn't abort the run
/// (ADR-0002), and collisions deliberately skipped because the [`CollisionPolicy`] declined
/// to overwrite them (issue #37 keeps these last two apart).
struct UnpackReport {
    restored_paths: Vec<PathBuf>,
    failures: Vec<RestoreFailure>,
    skipped_collisions: Vec<SkippedCollision>,
}

/// Resolves the whole-run overwrite decision (issue #37's "confirm once") before any file is
/// written. [`CollisionPolicy::Overwrite`] is an unconditional yes with no pre-scan;
/// [`CollisionPolicy::Confirm`] pre-scans every collision and, only when the list is
/// non-empty, calls the injected callback exactly once with the full list — so an interactive
/// prompt can name everything at stake in a single confirmation.
fn resolve_overwrite(
    archive_bytes: &[u8],
    ctx: &RunContext,
    policy: &mut CollisionPolicy<'_>,
) -> Result<bool, CoreError> {
    match policy {
        CollisionPolicy::Overwrite => Ok(true),
        CollisionPolicy::Confirm(confirm) => {
            let colliding = scan_collisions(archive_bytes, ctx)?;
            if colliding.is_empty() {
                Ok(false)
            } else {
                Ok(confirm(&colliding))
            }
        }
    }
}

/// Resolves every Archive entry's restore target and returns those that already exist — the
/// collision set the "confirm once" prompt names. A read-only dry pass: it resolves the same
/// targets [`unpack_all`] will, but writes nothing. Per-entry resolution errors are ignored
/// here (they surface as real failures in the actual unpack pass, so reporting them twice
/// would double-count); only an Archive-level decode error, which would fail the real pass
/// too, propagates.
fn scan_collisions(archive_bytes: &[u8], ctx: &RunContext) -> Result<Vec<PathBuf>, CoreError> {
    let zstd_decoder = zstd::stream::read::Decoder::new(archive_bytes)
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    let mut tar_archive = tar::Archive::new(zstd_decoder);
    let tar_entries = tar_archive
        .entries()
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;

    let mut colliding = Vec::new();
    for tar_entry in tar_entries {
        let Ok(tar_entry) = tar_entry else { continue };
        let Ok(entry_path_in_tar) = tar_entry.path().map(std::borrow::Cow::into_owned) else {
            continue;
        };
        let Ok(target) = resolve_target(&entry_path_in_tar, ctx) else {
            continue;
        };
        if target.exists() {
            colliding.push(target);
        }
    }
    Ok(colliding)
}

/// Untars every entry in the decrypted, decompressed-on-the-fly Archive bytes, Re-rooting
/// as it goes. Per ADR-0002, a per-file failure is collected and iteration continues.
///
/// `overwrite` is the whole-run decision already resolved by [`resolve_overwrite`]: when
/// `false`, a target that already exists is left untouched and recorded as a
/// [`SkippedCollision`] (issue #37 — a deliberate skip, not a failure); when `true`, it's
/// overwritten.
///
/// `total` is the Manifest's Entry count, used only to tick `progress` toward a known
/// denominator — an approximate total (the tar stream and the Manifest's Entry list needn't
/// agree exactly), which the [`Progress`] contract explicitly permits.
fn unpack_all(
    archive_bytes: &[u8],
    ctx: &RunContext,
    overwrite: bool,
    total: usize,
    progress: &mut dyn Progress,
) -> Result<UnpackReport, CoreError> {
    let zstd_decoder = zstd::stream::read::Decoder::new(archive_bytes)
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    let mut tar_archive = tar::Archive::new(zstd_decoder);
    let tar_entries = tar_archive
        .entries()
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;

    let mut restored_paths = Vec::new();
    let mut failures = Vec::new();
    let mut skipped_collisions = Vec::new();

    for (index, tar_entry) in tar_entries.enumerate() {
        // Ticked at the top of the body (mirroring `archive::build`) so it fires once per tar
        // entry regardless of which skip/continue path the iteration takes below.
        progress.on_entry(index + 1, total);
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

        // Story 54 / issue #37: the collision unit is the restore target path itself —
        // distinct from the Backup-dir-name collision (#19/#27), which guards the *output*
        // side of a backup run, not a restore target. A collision the overwrite decision
        // declined is a deliberate skip (recorded apart from real failures), never clobbered.
        if target.exists() && !overwrite {
            skipped_collisions.push(SkippedCollision {
                path: target.display().to_string(),
            });
            continue;
        }

        match unpack_one(&mut tar_entry, &target) {
            Ok(()) => {
                restored_paths.push(target.clone());
                // Story 53: attempt chown only as root; never even try (let alone fail
                // silently) as a non-root run — that decision is surfaced via
                // `RestoreOutcome::ownership` rather than re-derived per file here.
                if ctx.privilege == Privilege::Root
                    && let Err(reason) = chown_to_tar_owner(&tar_entry, &target)
                {
                    failures.push(RestoreFailure {
                        path: target.display().to_string(),
                        reason: format!("ownership restore failed: {reason}"),
                    });
                }
            }
            Err(source) => failures.push(RestoreFailure {
                path: target.display().to_string(),
                reason: source.to_string(),
            }),
        }
    }

    Ok(UnpackReport {
        restored_paths,
        failures,
        skipped_collisions,
    })
}

/// Attempts to set `target`'s ownership to the uid/gid recorded in `tar_entry`'s header — the
/// Archive's own tar stream, not the Manifest (CONTEXT.md: "No POSIX metadata table — tar
/// owns mode/uid/gid/symlink", so the tar header is the one source of truth for this,
/// deliberately not duplicated in the Manifest). A failure here (e.g. `EPERM` if the real
/// process privilege doesn't actually match `ctx.privilege`) is the caller's to fold into
/// the per-file failure report — it must never abort the run (ADR-0002).
///
/// Uses `lchown`, not `chown`: for a restored symlink Entry (issue #36) the ownership must
/// land on the link itself, never be followed to its target — `chown` would dereference the
/// link and either re-own an unrelated file or spuriously fail ENOENT on a dangling link.
/// `lchown` is identical to `chown` for a non-symlink target, so regular files/directories
/// are unaffected.
fn chown_to_tar_owner<R: std::io::Read>(
    tar_entry: &tar::Entry<'_, R>,
    target: &Path,
) -> Result<(), String> {
    let uid = tar_entry.header().uid().map_err(|e| e.to_string())?;
    let gid = tar_entry.header().gid().map_err(|e| e.to_string())?;
    // Linux uid/gid are 32-bit; tar's accessor widens them to u64, so the domain can't exceed u32.
    #[allow(clippy::cast_possible_truncation)]
    let (uid, gid) = (uid as u32, gid as u32);
    std::os::unix::fs::lchown(target, Some(uid), Some(gid)).map_err(|e| e.to_string())
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
        let Ok(target) = resolve_target(Path::new(archive_path), ctx) else {
            report.missing.push(archive_path.clone());
            continue;
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

    /// Whether the real test process can actually `chown` to an arbitrary foreign uid —
    /// true only when genuinely running as root (or with `CAP_CHOWN`). Guards
    /// `ownership_restore_failure_is_reported_without_aborting_the_restore` against running
    /// under a root CI container, where the real syscall this test depends on failing
    /// (EPERM) would instead succeed.
    fn process_can_chown_to_arbitrary_uid(probe_path: &Path) -> bool {
        std::os::unix::fs::chown(probe_path, Some(65_534), Some(65_534)).is_ok()
    }

    /// Story 53 / issue #26: `ctx.privilege` is `core`'s own logical flag — per
    /// `context.rs`'s doc comment, it "does not change what the real OS lets the real
    /// process do". So a test process that isn't actually root, given a `RunContext` that
    /// *claims* `Privilege::Root`, will hit a genuine `EPERM` from the real `chown(2)`
    /// syscall when the tar header names a uid/gid foreign to it. This proves, against a
    /// real failing syscall rather than a mocked one, that an ownership-restore failure
    /// lands in `failures` (with the file's content still restored, per ADR-0002) instead
    /// of aborting the run — the exact behavior `RestoreOutcome::ownership` /
    /// `OwnershipOutcome::Restored` promise to a caller.
    #[test]
    fn ownership_restore_failure_is_reported_without_aborting_the_restore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let restore_home = tmp.path().join("restored-home");
        fs::create_dir_all(&restore_home).expect("create restore home");

        // Running with real chown-anything privilege (e.g. a root CI container) would make the
        // foreign-uid chown below succeed instead of failing, which isn't this test's scenario
        // (story 53 is specifically about non-root's inability to chown) — skip rather than
        // assert a false failure. The normal (non-root) dev/CI process falls through and
        // exercises the real EPERM branch.
        if process_can_chown_to_arbitrary_uid(&restore_home) {
            eprintln!(
                "skipping ownership_restore_failure_is_reported_without_aborting_the_restore: \
                 process can chown arbitrary uids (running as root?)"
            );
            return;
        }

        // Build a minimal in-memory Archive (tar -> zstd) with one entry whose header
        // claims a uid/gid foreign to this process — `tar::Header::set_metadata` in
        // `archive::build` normally captures the *real* file owner, but a Backup restored
        // on a different machine/user (the whole point of Re-rooting) can easily carry a
        // uid that doesn't exist locally, which is exactly the scenario this reproduces.
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let data = b"hello from a foreign owner";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_uid(65_534);
            header.set_gid(65_534);
            header.set_cksum();
            builder
                .append_data(&mut header, "home/greeting.txt", &data[..])
                .expect("append tar entry");
            builder.finish().expect("finish tar archive");
        }
        let archive_bytes =
            zstd::stream::encode_all(&tar_bytes[..], 0).expect("zstd-compress tar bytes");

        let mut ctx = test_ctx(restore_home.clone());
        ctx.privilege = Privilege::Root;

        let UnpackReport {
            restored_paths,
            failures,
            ..
        } = unpack_all(
            &archive_bytes,
            &ctx,
            false,
            1,
            &mut crate::progress::NoopProgress,
        )
        .expect("unpack_all must not abort the run");

        let target = restore_home.join("greeting.txt");
        assert_eq!(
            restored_paths,
            vec![target.clone()],
            "content must still land even though ownership restore fails"
        );
        assert_eq!(
            fs::read(&target).expect("restored file should be readable"),
            b"hello from a foreign owner"
        );

        let ownership_failure = failures
            .iter()
            .find(|f| f.path == target.display().to_string())
            .expect("the chown failure must be reported in the per-file failure report");
        assert!(
            ownership_failure
                .reason
                .contains("ownership restore failed"),
            "failure reason should name it as an ownership failure: {}",
            ownership_failure.reason
        );
    }

    /// Builds a minimal in-memory Archive (tar -> zstd) with one file entry per `(tar_path,
    /// bytes)` pair, matching the on-disk shape `restore` consumes.
    fn build_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, &data[..])
                    .expect("append tar entry");
            }
            builder.finish().expect("finish tar archive");
        }
        zstd::stream::encode_all(&tar_bytes[..], 0).expect("zstd-compress tar bytes")
    }

    /// Issue #37: the interactive collision decision must be a *single* "confirm once" — the
    /// callback is invoked exactly once, with every colliding target at once, never per file.
    #[test]
    fn confirm_collision_callback_is_called_once_with_every_colliding_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        // Two targets that already exist (collisions) plus one that doesn't.
        fs::write(home.join("a.txt"), b"old a").expect("write a");
        fs::write(home.join("b.txt"), b"old b").expect("write b");

        let archive_bytes = build_archive(&[
            ("home/a.txt", b"new a"),
            ("home/b.txt", b"new b"),
            ("home/c.txt", b"new c"),
        ]);
        let ctx = test_ctx(home.clone());

        let mut calls: Vec<Vec<PathBuf>> = Vec::new();
        let mut confirm = |collisions: &[PathBuf]| {
            calls.push(collisions.to_vec());
            false
        };
        let mut policy = CollisionPolicy::Confirm(&mut confirm);
        let overwrite =
            resolve_overwrite(&archive_bytes, &ctx, &mut policy).expect("resolve must not error");

        assert!(!overwrite, "declining must resolve to no-overwrite");
        assert_eq!(
            calls.len(),
            1,
            "the confirmation must be asked exactly once"
        );
        let mut seen = calls[0].clone();
        seen.sort();
        assert_eq!(
            seen,
            vec![home.join("a.txt"), home.join("b.txt")],
            "the single prompt must name every colliding target and only those"
        );
    }

    /// Issue #37: with no collisions, the confirmation is never shown (nothing to ask about)
    /// and the run proceeds — the interactive path must not nag on a clean restore root.
    #[test]
    fn confirm_collision_callback_is_not_called_when_there_are_no_collisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("create home");

        let archive_bytes = build_archive(&[("home/fresh.txt", b"content")]);
        let ctx = test_ctx(home);

        let mut called = false;
        let mut confirm = |_: &[PathBuf]| {
            called = true;
            true
        };
        let mut policy = CollisionPolicy::Confirm(&mut confirm);
        let overwrite =
            resolve_overwrite(&archive_bytes, &ctx, &mut policy).expect("resolve must not error");

        assert!(!called, "no collisions means the prompt must never fire");
        assert!(
            !overwrite,
            "with nothing to overwrite, the resolved decision is a harmless no-overwrite"
        );
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
