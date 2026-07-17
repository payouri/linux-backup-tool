//! Output location enumeration: mounted drives via `sysinfo`, the `lsblk`/`udisksctl`
//! delta for unmounted USB partitions, and the always-offered `$HOME` row.
//!
//! Per issue #17's Testing Decisions, the real `sysinfo`-backed enumeration (the
//! [`discover_disks`] boundary) is a system-boundary edge that touches real hardware/mount
//! state; it stays off the round-trip seam and is not unit-tested in the alpha. Everything
//! else in this module — pick-list construction, filtering, preselection, custom-path
//! expansion, and location validation — is pure logic over an injected `&[DiskInfo]` (or
//! plain paths), kept deliberately thin-and-injectable so it *is* unit-tested.
//!
//! The `lsblk`/unmounted-USB delta and `udisksctl` mounting (stories 34-36) are explicitly
//! out of scope here — see issue #28.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// One disk/mount fact, shaped like `sysinfo::Disk` but decoupled from it so pick-list
/// logic can be exercised against fake data without touching real hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub mount_point: PathBuf,
    pub total_space: u64,
    pub available_space: u64,
    pub file_system: String,
    pub is_removable: bool,
    pub is_read_only: bool,
}

/// One row of the Output-location pick-list (story 25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationRow {
    /// `$HOME`, always offered (story 26).
    Home { path: PathBuf },
    /// A writable mount discovered under `/run/media/$USER`, `/media`, or `/mnt` (story 27).
    Mount {
        mount_point: PathBuf,
        file_system: String,
        available_space: u64,
        total_space: u64,
        is_removable: bool,
    },
    /// "↻ Rescan drives" (story 29).
    RescanDrives,
    /// "Custom path…" (story 30).
    CustomPath,
}

/// The assembled pick-list plus which row (if any) should start preselected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationPickList {
    pub rows: Vec<LocationRow>,
    /// Index into `rows` of the sole external drive, when exactly one qualifying
    /// removable mount is present (story 28). `None` otherwise — never guess among
    /// several candidates.
    pub preselected: Option<usize>,
}

/// The root-partition mount point, hidden from the pick-list (story 27).
const ROOT_MOUNT: &str = "/";
/// The EFI system partition's conventional mount point, hidden from the pick-list
/// (story 27). Other ESP mount points aren't recognized in the alpha.
const EFI_MOUNT: &str = "/boot/efi";

/// Whether `mount_point` is the root or EFI partition, which the pick-list must hide
/// even if it otherwise looks like a plausible target (story 27).
fn is_root_or_efi(mount_point: &Path) -> bool {
    mount_point == Path::new(ROOT_MOUNT) || mount_point == Path::new(EFI_MOUNT)
}

/// Whether `mount_point` sits under one of the three prefixes story 27 calls out:
/// `/run/media/$USER`, `/media`, or `/mnt`.
fn is_candidate_mount(mount_point: &Path, user: &str) -> bool {
    let run_media_user = PathBuf::from("/run/media").join(user);
    mount_point.starts_with(&run_media_user)
        || mount_point.starts_with("/media")
        || mount_point.starts_with("/mnt")
}

/// Build the Output-location pick-list from injected disk data (story 25).
///
/// Always includes a `$HOME` row, then every writable candidate mount (sorted by mount
/// point for determinism), then `RescanDrives`, then `CustomPath`. Root/EFI partitions and
/// non-writable mounts are hidden (story 27); the sole qualifying external drive is
/// preselected (story 28).
pub fn build_pick_list(disks: &[DiskInfo], home: &Path, user: &str) -> LocationPickList {
    let mut candidates: Vec<&DiskInfo> = disks
        .iter()
        .filter(|d| !d.is_read_only)
        .filter(|d| !is_root_or_efi(&d.mount_point))
        .filter(|d| is_candidate_mount(&d.mount_point, user))
        .collect();
    candidates.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));

    let mut rows = Vec::with_capacity(candidates.len() + 3);
    rows.push(LocationRow::Home {
        path: home.to_path_buf(),
    });

    let mut removable_indices = Vec::new();
    for disk in &candidates {
        if disk.is_removable {
            removable_indices.push(rows.len());
        }
        rows.push(LocationRow::Mount {
            mount_point: disk.mount_point.clone(),
            file_system: disk.file_system.clone(),
            available_space: disk.available_space,
            total_space: disk.total_space,
            is_removable: disk.is_removable,
        });
    }

    rows.push(LocationRow::RescanDrives);
    rows.push(LocationRow::CustomPath);

    let preselected = match removable_indices.as_slice() {
        [only] => Some(*only),
        _ => None,
    };

    LocationPickList { rows, preselected }
}

/// Expand a leading `~` (bare, or `~/...`) to `home`. Other users' homes (`~otheruser`)
/// aren't resolved in the alpha and are left literal.
fn expand_tilde(input: &str, home: &Path) -> PathBuf {
    if input == "~" {
        home.to_path_buf()
    } else if let Some(rest) = input.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(input)
    }
}

/// Expand `$VAR` and `${VAR}` references using the injected `getenv` lookup. Per
/// ADR-0004, `core` never reads `std::env` itself — the caller (`crates/tui`'s `main`, or
/// a test) supplies the environment snapshot. An unresolvable variable is left literal
/// rather than erroring, so a typo is obvious rather than silently vanishing into an
/// unexpected path.
fn expand_env_vars(input: &str, getenv: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    closed = true;
                    break;
                }
                name.push(c2);
            }
            match getenv(&name) {
                Some(val) => out.push_str(&val),
                None => {
                    out.push_str("${");
                    out.push_str(&name);
                    if closed {
                        out.push('}');
                    }
                }
            }
        } else {
            let mut name = String::new();
            while let Some(&c2) = chars.peek() {
                if c2.is_alphanumeric() || c2 == '_' {
                    name.push(c2);
                    chars.next();
                } else {
                    break;
                }
            }
            if name.is_empty() {
                out.push('$');
            } else {
                match getenv(&name) {
                    Some(val) => out.push_str(&val),
                    None => {
                        out.push('$');
                        out.push_str(&name);
                    }
                }
            }
        }
    }
    out
}

/// Expand `~`/`$VARS` in a user-typed Custom path (story 30), without touching the
/// filesystem. Env-var expansion runs before tilde expansion so `$HOME/foo` and `~/foo`
/// both land under `home`.
pub fn expand_path(input: &str, home: &Path, getenv: impl Fn(&str) -> Option<String>) -> PathBuf {
    let with_vars = expand_env_vars(input, &getenv);
    expand_tilde(&with_vars, home)
}

/// Expand a Custom path and create it if missing (story 30). The directory is created
/// with `create_dir_all` so intermediate components are made too.
pub fn resolve_custom_path(
    input: &str,
    home: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> Result<PathBuf, CoreError> {
    let expanded = expand_path(input, home, getenv);
    fs::create_dir_all(&expanded).map_err(|source| CoreError::Io {
        path: expanded.clone(),
        source,
    })?;
    Ok(expanded)
}

/// Probe filename used by [`ensure_writable`]; namespaced and pid-suffixed so concurrent
/// runs against the same location can't collide.
fn write_probe_name() -> String {
    format!(".linux-backup-write-check-{}", std::process::id())
}

/// Hard-refuse an unwritable Output location (story 31) by attempting a real write-then-
/// remove probe file — the same operation `core` will later perform for real, so a
/// permissions problem is caught here rather than mid-backup.
pub fn ensure_writable(path: &Path) -> Result<(), CoreError> {
    if !path.is_dir() {
        return Err(CoreError::LocationNotFound(path.to_path_buf()));
    }
    let probe = path.join(write_probe_name());
    match fs::write(&probe, []) {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(source) if source.kind() == ErrorKind::PermissionDenied => {
            Err(CoreError::LocationNotWritable(path.to_path_buf()))
        }
        Err(source) => Err(CoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Refuse if the computed Backup dir name already exists at `location` (story 33 — the
/// Backup dir is the collision unit). Returns the Backup dir's full path on success.
pub fn ensure_backup_dir_available(
    location: &Path,
    backup_dir_name: &str,
) -> Result<PathBuf, CoreError> {
    let target = location.join(backup_dir_name);
    if target.exists() {
        return Err(CoreError::BackupDirExists(target));
    }
    Ok(target)
}

/// A non-blocking warning that the Selection's uncompressed size approaches the Output
/// location's free space (story 32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpaceWarning {
    pub selection_size: u64,
    pub available_space: u64,
}

/// Warn (never block) once the Selection's uncompressed size reaches this fraction of the
/// Output location's free space.
const FREE_SPACE_WARNING_RATIO: f64 = 0.9;

/// Compute the free-space warning, if any (story 32) — pure comparison, never a hard
/// block. `available_space == 0` always warns (division by zero would otherwise hide the
/// worst case).
pub fn free_space_warning(selection_size: u64, available_space: u64) -> Option<FreeSpaceWarning> {
    let approaching = if available_space == 0 {
        true
    } else {
        (selection_size as f64 / available_space as f64) >= FREE_SPACE_WARNING_RATIO
    };
    approaching.then_some(FreeSpaceWarning {
        selection_size,
        available_space,
    })
}

/// The fully validated Output location: hard checks passed, plus a non-blocking
/// free-space warning when relevant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationSelection {
    pub location: PathBuf,
    pub backup_dir: PathBuf,
    pub warning: Option<FreeSpaceWarning>,
}

/// Validate a chosen Output location end-to-end: hard-refuse if unwritable (story 31),
/// hard-refuse if the Backup dir name collides (story 33), and compute the non-blocking
/// free-space warning (story 32) when `available_space` is known.
pub fn resolve_selection(
    location: &Path,
    backup_dir_name: &str,
    selection_size: u64,
    available_space: Option<u64>,
) -> Result<LocationSelection, CoreError> {
    ensure_writable(location)?;
    let backup_dir = ensure_backup_dir_available(location, backup_dir_name)?;
    let warning = available_space.and_then(|avail| free_space_warning(selection_size, avail));
    Ok(LocationSelection {
        location: location.to_path_buf(),
        backup_dir,
        warning,
    })
}

/// Enumerate real mounted disks via `sysinfo`. This is the system-boundary edge the
/// Testing Decisions call out as touching real hardware/mount state — not unit-tested in
/// the alpha (see this module's doc comment), and deliberately thin: every other function
/// in this module takes `&[DiskInfo]` rather than calling this itself.
pub fn discover_disks() -> Vec<DiskInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .map(|d| DiskInfo {
            mount_point: d.mount_point().to_path_buf(),
            total_space: d.total_space(),
            available_space: d.available_space(),
            file_system: d.file_system().to_string_lossy().into_owned(),
            is_removable: d.is_removable(),
            is_read_only: d.is_read_only(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(mount_point: &str, removable: bool, read_only: bool) -> DiskInfo {
        DiskInfo {
            mount_point: PathBuf::from(mount_point),
            total_space: 1_000_000_000,
            available_space: 500_000_000,
            file_system: "ext4".to_string(),
            is_removable: removable,
            is_read_only: read_only,
        }
    }

    // --- build_pick_list -----------------------------------------------------------

    #[test]
    fn home_row_always_present_even_with_no_disks() {
        let home = PathBuf::from("/home/youri");
        let picks = build_pick_list(&[], &home, "youri");
        assert_eq!(picks.rows[0], LocationRow::Home { path: home });
    }

    #[test]
    fn rescan_and_custom_path_rows_always_present() {
        let home = PathBuf::from("/home/youri");
        let picks = build_pick_list(&[], &home, "youri");
        assert!(picks.rows.contains(&LocationRow::RescanDrives));
        assert!(picks.rows.contains(&LocationRow::CustomPath));
    }

    #[test]
    fn root_and_efi_partitions_are_hidden() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/", false, false), disk("/boot/efi", false, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn writable_mount_under_run_media_user_is_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert!(picks.rows.iter().any(
            |r| matches!(r, LocationRow::Mount { mount_point, .. } if mount_point == Path::new("/run/media/youri/USBSTICK"))
        ));
    }

    #[test]
    fn run_media_mount_under_a_different_user_is_not_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/otheruser/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn media_and_mnt_mounts_are_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![
            disk("/media/backup", true, false),
            disk("/mnt/data", false, false),
        ];
        let picks = build_pick_list(&disks, &home, "youri");
        let mounts: Vec<_> = picks
            .rows
            .iter()
            .filter(|r| matches!(r, LocationRow::Mount { .. }))
            .collect();
        assert_eq!(mounts.len(), 2);
    }

    #[test]
    fn a_mount_elsewhere_on_the_filesystem_is_not_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/srv/data", true, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn read_only_mount_is_hidden() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/RODISK", true, true)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn sole_external_drive_is_preselected() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        let idx = picks.preselected.expect("expected a preselection");
        assert!(matches!(picks.rows[idx], LocationRow::Mount { .. }));
    }

    #[test]
    fn two_external_drives_preselect_neither() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![
            disk("/run/media/youri/USBSTICK1", true, false),
            disk("/run/media/youri/USBSTICK2", true, false),
        ];
        let picks = build_pick_list(&disks, &home, "youri");
        assert_eq!(picks.preselected, None);
    }

    #[test]
    fn non_removable_candidate_mount_is_not_preselected() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/mnt/data", false, false)];
        let picks = build_pick_list(&disks, &home, "youri");
        assert_eq!(picks.preselected, None);
    }

    // --- expand_path (tilde / env vars) ---------------------------------------------

    #[test]
    fn bare_tilde_expands_to_home() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("~", &home, |_| None);
        assert_eq!(out, home);
    }

    #[test]
    fn tilde_slash_expands_relative_to_home() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("~/backups", &home, |_| None);
        assert_eq!(out, PathBuf::from("/home/youri/backups"));
    }

    #[test]
    fn dollar_var_is_expanded() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("$TARGET/backups", &home, |name| {
            (name == "TARGET").then(|| "/mnt/data".to_string())
        });
        assert_eq!(out, PathBuf::from("/mnt/data/backups"));
    }

    #[test]
    fn braced_var_is_expanded() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("${TARGET}/backups", &home, |name| {
            (name == "TARGET").then(|| "/mnt/data".to_string())
        });
        assert_eq!(out, PathBuf::from("/mnt/data/backups"));
    }

    #[test]
    fn unresolved_var_is_left_literal() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("$NOPE/backups", &home, |_| None);
        assert_eq!(out, PathBuf::from("$NOPE/backups"));
    }

    #[test]
    fn env_expansion_runs_before_tilde_so_home_var_composes() {
        let home = PathBuf::from("/home/youri");
        let out = expand_path("$HOME/backups", &home, |name| {
            (name == "HOME").then(|| "~".to_string())
        });
        assert_eq!(out, home.join("backups"));
    }

    // --- resolve_custom_path (create-if-missing) ------------------------------------

    #[test]
    fn resolve_custom_path_creates_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let target = tmp.path().join("nested/does/not/exist");
        assert!(!target.exists());
        let resolved = resolve_custom_path(target.to_str().unwrap(), &home, |_| None).unwrap();
        assert_eq!(resolved, target);
        assert!(target.is_dir());
    }

    #[test]
    fn resolve_custom_path_expands_tilde_and_creates() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let resolved = resolve_custom_path("~/my-backups", &home, |_| None).unwrap();
        assert_eq!(resolved, home.join("my-backups"));
        assert!(resolved.is_dir());
    }

    // --- ensure_writable (hard refusal) ---------------------------------------------

    #[test]
    fn writable_directory_passes() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_writable(tmp.path()).is_ok());
    }

    #[test]
    fn nonexistent_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = ensure_writable(&missing).unwrap_err();
        assert!(matches!(err, CoreError::LocationNotFound(p) if p == missing));
    }

    #[test]
    fn unwritable_directory_is_hard_refused() {
        // Root bypasses permission bits entirely, which would make this test flaky
        // under CI-as-root; skip in that case (same pattern ADR-0004 documents for the
        // permission-denied backup test). This reads the *test process's* real euid to
        // decide whether to skip — it is test infrastructure, not core production logic,
        // so it doesn't reopen ADR-0004's ban on core reading ambient facts.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
        let err = ensure_writable(&locked);
        // restore permissions so tempdir cleanup can remove it
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, Err(CoreError::LocationNotWritable(p)) if p == locked));
    }

    // --- ensure_backup_dir_available (collision refusal) ----------------------------

    #[test]
    fn fresh_backup_dir_name_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        let target = ensure_backup_dir_available(tmp.path(), "linux-backup-host-1").unwrap();
        assert_eq!(target, tmp.path().join("linux-backup-host-1"));
    }

    #[test]
    fn existing_backup_dir_name_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("linux-backup-host-1")).unwrap();
        let err = ensure_backup_dir_available(tmp.path(), "linux-backup-host-1").unwrap_err();
        assert!(
            matches!(err, CoreError::BackupDirExists(p) if p == tmp.path().join("linux-backup-host-1"))
        );
    }

    // --- free_space_warning ----------------------------------------------------------

    #[test]
    fn plenty_of_free_space_warns_never() {
        assert_eq!(free_space_warning(1_000, 1_000_000), None);
    }

    #[test]
    fn tight_but_sufficient_space_warns() {
        let warning = free_space_warning(950, 1_000).expect("expected a warning");
        assert_eq!(warning.selection_size, 950);
        assert_eq!(warning.available_space, 1_000);
    }

    #[test]
    fn selection_exceeding_free_space_still_only_warns_not_blocks() {
        // Exceeding available space is a warning, never an error return, per story 32.
        let warning = free_space_warning(2_000, 1_000);
        assert!(warning.is_some());
    }

    #[test]
    fn zero_available_space_warns() {
        assert!(free_space_warning(1, 0).is_some());
    }

    // --- resolve_selection (composition) --------------------------------------------

    #[test]
    fn resolve_selection_succeeds_with_no_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let selection =
            resolve_selection(tmp.path(), "linux-backup-host-1", 10, Some(1_000_000)).unwrap();
        assert_eq!(selection.location, tmp.path());
        assert_eq!(selection.backup_dir, tmp.path().join("linux-backup-host-1"));
        assert_eq!(selection.warning, None);
    }

    #[test]
    fn resolve_selection_surfaces_free_space_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let selection =
            resolve_selection(tmp.path(), "linux-backup-host-1", 950, Some(1_000)).unwrap();
        assert!(selection.warning.is_some());
    }

    #[test]
    fn resolve_selection_refuses_backup_dir_collision() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("linux-backup-host-1")).unwrap();
        let err = resolve_selection(tmp.path(), "linux-backup-host-1", 10, None).unwrap_err();
        assert!(matches!(err, CoreError::BackupDirExists(_)));
    }

    #[test]
    fn resolve_selection_hard_refuses_unwritable_location() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let err = resolve_selection(&missing, "linux-backup-host-1", 10, None).unwrap_err();
        assert!(matches!(err, CoreError::LocationNotFound(_)));
    }

    // --- discover_disks (smoke only; the real boundary is not unit-tested) ----------

    #[test]
    fn discover_disks_runs_without_panicking() {
        // Per the Testing Decisions, the sysinfo-backed boundary itself isn't asserted
        // against — this just proves the thin wrapper doesn't panic on this machine.
        let _ = discover_disks();
    }
}
