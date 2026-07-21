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
//! Issue #28 adds the same shape for stories 34-36: [`run_lsblk`] and [`mount_partition`]
//! are the thin real-subprocess boundaries (not unit-tested, mirroring [`discover_disks`]),
//! while [`parse_lsblk_unmounted_usb_partitions`] and the mount-outcome parsing are pure
//! functions over captured text and so are unit-tested against fixtures.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

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
    /// A USB partition `lsblk` reports with no mountpoint yet — shown below every mounted
    /// row, never above (story 34). Selecting it drives [`mount_partition`]; the resulting
    /// re-scan is what moves it into an ordinary `Mount` row (story 35), not any special
    /// casing here.
    UnmountedUsbPartition {
        device: PathBuf,
        size: u64,
        fstype: Option<String>,
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

/// One USB-attached partition `lsblk` reports as not yet mounted (story 34). Shaped like
/// [`DiskInfo`]'s injection pattern: pick-list logic takes this as plain data so it's
/// exercised against fixtures rather than a running `lsblk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmountedPartition {
    pub device: PathBuf,
    pub size: u64,
    pub fstype: Option<String>,
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

/// Build the Output-location pick-list from injected disk data (story 25) plus the
/// injected unmounted-USB-partition delta (story 34).
///
/// Always includes a `$HOME` row, then every writable candidate mount (sorted by mount
/// point for determinism), then unmounted USB partitions (sorted by device for the same
/// reason, below every mounted row per story 34), then `RescanDrives`, then `CustomPath`.
/// Root/EFI partitions and non-writable mounts are hidden (story 27); the sole qualifying
/// external drive is preselected (story 28). `unmounted` is expected to already exclude
/// LUKS/LVM member partitions — see [`parse_lsblk_unmounted_usb_partitions`], the one place
/// that filtering happens — so this function doesn't re-check `fstype`.
pub fn build_pick_list(
    disks: &[DiskInfo],
    unmounted: &[UnmountedPartition],
    home: &Path,
    user: &str,
) -> LocationPickList {
    let mut candidates: Vec<&DiskInfo> = disks
        .iter()
        .filter(|d| !d.is_read_only)
        .filter(|d| !is_root_or_efi(&d.mount_point))
        .filter(|d| is_candidate_mount(&d.mount_point, user))
        .collect();
    candidates.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));

    let mut sorted_unmounted: Vec<&UnmountedPartition> = unmounted.iter().collect();
    sorted_unmounted.sort_by(|a, b| a.device.cmp(&b.device));

    let mut rows = Vec::with_capacity(candidates.len() + sorted_unmounted.len() + 3);
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

    for partition in &sorted_unmounted {
        rows.push(LocationRow::UnmountedUsbPartition {
            device: partition.device.clone(),
            size: partition.size,
            fstype: partition.fstype.clone(),
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
            if let Some(val) = getenv(&name) {
                out.push_str(&val);
            } else {
                out.push_str("${");
                out.push_str(&name);
                if closed {
                    out.push('}');
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
            } else if let Some(val) = getenv(&name) {
                out.push_str(&val);
            } else {
                out.push('$');
                out.push_str(&name);
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
    // Disk byte counts stay under 2^52, so f64 loss is negligible for this human-facing ratio.
    #[allow(clippy::cast_precision_loss)]
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

/// Mirrors `lsblk --json -b -o NAME,TYPE,TRAN,MOUNTPOINT,SIZE,FSTYPE,PATH`'s shape closely
/// enough to deserialize it; kept private since callers only need the filtered result from
/// [`parse_lsblk_unmounted_usb_partitions`], not `lsblk`'s full device tree.
#[derive(Debug, Deserialize)]
struct LsblkOutput {
    blockdevices: Vec<LsblkDevice>,
}

#[derive(Debug, Deserialize)]
struct LsblkDevice {
    path: Option<String>,
    #[serde(rename = "type")]
    device_type: Option<String>,
    tran: Option<String>,
    mountpoint: Option<String>,
    size: Option<u64>,
    fstype: Option<String>,
    #[serde(default)]
    children: Vec<Self>,
}

/// fstype signatures that mean the partition is a LUKS container or an LVM physical
/// volume rather than a directly mountable filesystem — excluded even though they can pass
/// `tran==usb && type==part && mountpoint==null` (story 28's "excluding ... LUKS, LVM").
/// Recent `lsblk` propagates the parent disk's `tran` down to partition children, which is
/// what makes filtering flat over every row (not just top-level disks) correct here.
fn is_unmounted_usb_partition(dev: &LsblkDevice) -> bool {
    dev.device_type.as_deref() == Some("part")
        && dev.tran.as_deref() == Some("usb")
        && dev.mountpoint.is_none()
        && !matches!(dev.fstype.as_deref(), Some("crypto_LUKS" | "LVM2_member"))
}

/// Walk `lsblk`'s nested `children` arrays collecting matches, since a USB stick's
/// partitions are reported as children of its disk entry rather than flattened at the top
/// level.
fn collect_unmounted_usb_partitions(devices: &[LsblkDevice], out: &mut Vec<UnmountedPartition>) {
    for dev in devices {
        if is_unmounted_usb_partition(dev)
            && let Some(path) = &dev.path
        {
            out.push(UnmountedPartition {
                device: PathBuf::from(path),
                size: dev.size.unwrap_or(0),
                fstype: dev.fstype.clone(),
            });
        }
        collect_unmounted_usb_partitions(&dev.children, out);
    }
}

/// Parse `lsblk --json -b` output into the unmounted USB partitions story 34 wants offered
/// (`tran==usb && type==part && mountpoint==null`), excluding internal spares (no `usb`
/// transport), LUKS, LVM, and network shares (none of which report `tran==usb`). Pure over
/// the given text so it's unit-testable against fixture JSON without a real `lsblk`.
pub fn parse_lsblk_unmounted_usb_partitions(
    json: &str,
) -> Result<Vec<UnmountedPartition>, CoreError> {
    let parsed: LsblkOutput =
        serde_json::from_str(json).map_err(|e| CoreError::LsblkParseFailed(e.to_string()))?;
    let mut out = Vec::new();
    collect_unmounted_usb_partitions(&parsed.blockdevices, &mut out);
    Ok(out)
}

/// Run the real `lsblk` subprocess (story 34). Mirrors [`discover_disks`] as the
/// system-boundary edge the Testing Decisions call out — not unit-tested in the alpha;
/// every other function here takes parsed/injected data instead of calling this itself.
pub fn run_lsblk() -> Result<Vec<UnmountedPartition>, CoreError> {
    let output = Command::new("lsblk")
        .args([
            "--json",
            "-b",
            "-o",
            "NAME,TYPE,TRAN,MOUNTPOINT,SIZE,FSTYPE,PATH",
        ])
        .output()
        .map_err(|source| CoreError::Io {
            path: PathBuf::from("lsblk"),
            source,
        })?;
    if !output.status.success() {
        return Err(CoreError::LsblkCommandFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    parse_lsblk_unmounted_usb_partitions(&String::from_utf8_lossy(&output.stdout))
}

/// A `udisksctl mount` that succeeded (story 35): the device and where it landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedPartition {
    pub device: PathBuf,
    pub mount_point: PathBuf,
}

/// Why a `udisksctl mount` attempt failed (story 36). `udisksctl` has no machine-readable
/// error format, so this is necessarily a best-effort classification of its stderr text —
/// never a reason to crash or exit; the caller shows an inline message and re-lists.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MountError {
    /// Polkit denied the action (or couldn't auto-authenticate under
    /// `--no-user-interaction`) — observed as `org.freedesktop.UDisks2.Error.NotAuthorized`
    /// in `udisksctl`'s stderr.
    #[error("not authorized to mount {device}")]
    NotAuthorized { device: PathBuf },
    /// The device vanished between being listed and being mounted (unplugged mid-flow) —
    /// observed as "Error looking up object for device ..." in `udisksctl`'s stderr.
    #[error("device {device} is gone")]
    DeviceGone { device: PathBuf },
    /// Any other failure, carrying `udisksctl`'s stderr verbatim since the alpha has no
    /// richer classification for it.
    #[error("failed to mount {device}: {message}")]
    Other { device: PathBuf, message: String },
}

/// Parse `udisksctl mount`'s success line (observed shape: `Mounted <dev> at <path>.`).
/// Pure over captured stdout so it's unit-testable without a real mount.
fn parse_mount_success(stdout: &str, device: &Path) -> Option<MountedPartition> {
    let line = stdout.lines().find(|l| l.starts_with("Mounted "))?;
    let (_, after) = line.strip_prefix("Mounted ")?.split_once(" at ")?;
    Some(MountedPartition {
        device: device.to_path_buf(),
        mount_point: PathBuf::from(after.trim().trim_end_matches('.')),
    })
}

/// Classify a `udisksctl mount` failure from its stderr text (story 36). Pure over captured
/// text so it's unit-testable against representative `udisksctl` output.
fn classify_mount_failure(device: &Path, stderr: &str) -> MountError {
    if stderr.contains("NotAuthorized") || stderr.contains("Not authorized") {
        MountError::NotAuthorized {
            device: device.to_path_buf(),
        }
    } else if stderr.contains("Error looking up object for device") {
        MountError::DeviceGone {
            device: device.to_path_buf(),
        }
    } else {
        MountError::Other {
            device: device.to_path_buf(),
            message: stderr.trim().to_string(),
        }
    }
}

/// Run the real `udisksctl mount -b <device> --no-user-interaction` subprocess (stories
/// 35-36). Mirrors [`discover_disks`] and [`run_lsblk`] as a system-boundary edge — not
/// unit-tested in the alpha; [`parse_mount_success`] and [`classify_mount_failure`] carry
/// the testable parsing.
pub fn mount_partition(device: &Path) -> Result<MountedPartition, MountError> {
    let output = Command::new("udisksctl")
        .arg("mount")
        .arg("-b")
        .arg(device)
        .arg("--no-user-interaction")
        .output()
        .map_err(|source| MountError::Other {
            device: device.to_path_buf(),
            message: source.to_string(),
        })?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return parse_mount_success(&stdout, device).ok_or_else(|| MountError::Other {
            device: device.to_path_buf(),
            message: format!("unrecognized udisksctl output: {stdout}"),
        });
    }

    Err(classify_mount_failure(
        device,
        &String::from_utf8_lossy(&output.stderr),
    ))
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
        let picks = build_pick_list(&[], &[], &home, "youri");
        assert_eq!(picks.rows[0], LocationRow::Home { path: home });
    }

    #[test]
    fn rescan_and_custom_path_rows_always_present() {
        let home = PathBuf::from("/home/youri");
        let picks = build_pick_list(&[], &[], &home, "youri");
        assert!(picks.rows.contains(&LocationRow::RescanDrives));
        assert!(picks.rows.contains(&LocationRow::CustomPath));
    }

    #[test]
    fn root_and_efi_partitions_are_hidden() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/", false, false), disk("/boot/efi", false, false)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn writable_mount_under_run_media_user_is_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
        assert!(picks.rows.iter().any(
            |r| matches!(r, LocationRow::Mount { mount_point, .. } if mount_point == Path::new("/run/media/youri/USBSTICK"))
        ));
    }

    #[test]
    fn run_media_mount_under_a_different_user_is_not_offered() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/otheruser/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
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
        let picks = build_pick_list(&disks, &[], &home, "youri");
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
        let picks = build_pick_list(&disks, &[], &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn read_only_mount_is_hidden() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/RODISK", true, true)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));
    }

    #[test]
    fn sole_external_drive_is_preselected() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/USBSTICK", true, false)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
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
        let picks = build_pick_list(&disks, &[], &home, "youri");
        assert_eq!(picks.preselected, None);
    }

    #[test]
    fn non_removable_candidate_mount_is_not_preselected() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/mnt/data", false, false)];
        let picks = build_pick_list(&disks, &[], &home, "youri");
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
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses permission bits entirely, which would make this test flaky
        // under CI-as-root; skip in that case (same pattern ADR-0004 documents for the
        // permission-denied backup test). This reads the *test process's* real euid to
        // decide whether to skip — it is test infrastructure, not core production logic,
        // so it doesn't reopen ADR-0004's ban on core reading ambient facts.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
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

    // --- build_pick_list: unmounted USB partitions (story 34) -----------------------

    fn unmounted(device: &str, size: u64) -> UnmountedPartition {
        UnmountedPartition {
            device: PathBuf::from(device),
            size,
            fstype: Some("vfat".to_string()),
        }
    }

    #[test]
    fn unmounted_usb_partition_appears_as_a_distinct_row() {
        let home = PathBuf::from("/home/youri");
        let parts = vec![unmounted("/dev/sdb1", 8_000_000_000)];
        let picks = build_pick_list(&[], &parts, &home, "youri");
        assert!(picks.rows.iter().any(
            |r| matches!(r, LocationRow::UnmountedUsbPartition { device, .. } if device == Path::new("/dev/sdb1"))
        ));
    }

    #[test]
    fn unmounted_usb_partition_rows_sit_below_every_mounted_row() {
        let home = PathBuf::from("/home/youri");
        let disks = vec![disk("/run/media/youri/USBSTICK", true, false)];
        let parts = vec![unmounted("/dev/sdc1", 4_000_000_000)];
        let picks = build_pick_list(&disks, &parts, &home, "youri");
        let mount_idx = picks
            .rows
            .iter()
            .position(|r| matches!(r, LocationRow::Mount { .. }))
            .expect("expected a mounted row");
        let unmounted_idx = picks
            .rows
            .iter()
            .position(|r| matches!(r, LocationRow::UnmountedUsbPartition { .. }))
            .expect("expected an unmounted-partition row");
        assert!(unmounted_idx > mount_idx);
    }

    #[test]
    fn unmounted_usb_partition_rows_sit_above_rescan_and_custom_path() {
        let home = PathBuf::from("/home/youri");
        let parts = vec![unmounted("/dev/sdb1", 8_000_000_000)];
        let picks = build_pick_list(&[], &parts, &home, "youri");
        let unmounted_idx = picks
            .rows
            .iter()
            .position(|r| matches!(r, LocationRow::UnmountedUsbPartition { .. }))
            .unwrap();
        let rescan_idx = picks
            .rows
            .iter()
            .position(|r| matches!(r, LocationRow::RescanDrives))
            .unwrap();
        assert!(unmounted_idx < rescan_idx);
    }

    #[test]
    fn no_unmounted_partitions_means_no_such_rows() {
        let home = PathBuf::from("/home/youri");
        let picks = build_pick_list(&[], &[], &home, "youri");
        assert!(!picks
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::UnmountedUsbPartition { .. })));
    }

    // --- the re-list invariant (story 35): a plain rescan reflects the new mount state ---

    #[test]
    fn mounting_a_partition_moves_it_from_unmounted_to_a_normal_mounted_row_on_rescan() {
        let home = PathBuf::from("/home/youri");

        // Before: udisksctl hasn't mounted anything yet, so the stick only shows up via
        // the lsblk delta as an unmounted USB partition, not as a DiskInfo.
        let before = build_pick_list(
            &[],
            &[unmounted("/dev/sdb1", 8_000_000_000)],
            &home,
            "youri",
        );
        assert!(before
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::UnmountedUsbPartition { .. })));
        assert!(!before
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::Mount { .. })));

        // After: per #28's scope, core builds no "insert the newly mounted drive at the
        // cursor" mechanism (that's presentation-layer, #32) — the caller just re-scans.
        // A fresh discover_disks() would now report the stick as a normal DiskInfo, and a
        // fresh lsblk scan no longer reports it as unmounted (mountpoint is now set), so
        // this asserts a plain rescan over that new data reflects both halves of the
        // invariant without any special-casing.
        let after = build_pick_list(
            &[disk("/run/media/youri/USBSTICK", true, false)],
            &[],
            &home,
            "youri",
        );
        assert!(after.rows.iter().any(
            |r| matches!(r, LocationRow::Mount { mount_point, .. } if mount_point == Path::new("/run/media/youri/USBSTICK"))
        ));
        assert!(!after
            .rows
            .iter()
            .any(|r| matches!(r, LocationRow::UnmountedUsbPartition { .. })));
    }

    // --- parse_lsblk_unmounted_usb_partitions (pure, fixture-driven) -----------------

    /// Realistic `lsblk --json -b -o NAME,TYPE,TRAN,MOUNTPOINT,SIZE,FSTYPE,PATH` output:
    /// an internal `NVMe` disk (tran propagates to its partitions, none of them `usb`), a
    /// mounted USB stick, an unmounted USB stick, a USB-attached LUKS container (excluded
    /// per story 28's "excluding ... LUKS"), and a USB-attached LVM physical volume
    /// (excluded per "excluding ... LVM").
    const LSBLK_FIXTURE: &str = r#"{
       "blockdevices": [
          {
             "name": "nvme0n1", "type": "disk", "tran": "nvme", "mountpoint": null,
             "size": 1024209543168, "fstype": null, "path": "/dev/nvme0n1",
             "children": [
                { "name": "nvme0n1p1", "type": "part", "tran": "nvme", "mountpoint": "/",
                  "size": 611093315584, "fstype": "ext4", "path": "/dev/nvme0n1p1" }
             ]
          },
          {
             "name": "sda", "type": "disk", "tran": "usb", "mountpoint": null,
             "size": 16000000000, "fstype": null, "path": "/dev/sda",
             "children": [
                { "name": "sda1", "type": "part", "tran": "usb",
                  "mountpoint": "/run/media/youri/MOUNTED",
                  "size": 16000000000, "fstype": "vfat", "path": "/dev/sda1" }
             ]
          },
          {
             "name": "sdb", "type": "disk", "tran": "usb", "mountpoint": null,
             "size": 8000000000, "fstype": null, "path": "/dev/sdb",
             "children": [
                { "name": "sdb1", "type": "part", "tran": "usb", "mountpoint": null,
                  "size": 8000000000, "fstype": "vfat", "path": "/dev/sdb1" }
             ]
          },
          {
             "name": "sdc", "type": "disk", "tran": "usb", "mountpoint": null,
             "size": 32000000000, "fstype": null, "path": "/dev/sdc",
             "children": [
                { "name": "sdc1", "type": "part", "tran": "usb", "mountpoint": null,
                  "size": 32000000000, "fstype": "crypto_LUKS", "path": "/dev/sdc1" }
             ]
          },
          {
             "name": "sdd", "type": "disk", "tran": "usb", "mountpoint": null,
             "size": 64000000000, "fstype": null, "path": "/dev/sdd",
             "children": [
                { "name": "sdd1", "type": "part", "tran": "usb", "mountpoint": null,
                  "size": 64000000000, "fstype": "LVM2_member", "path": "/dev/sdd1" }
             ]
          }
       ]
    }"#;

    #[test]
    fn parses_the_one_genuinely_unmounted_usb_partition() {
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].device, PathBuf::from("/dev/sdb1"));
        assert_eq!(parts[0].size, 8_000_000_000);
        assert_eq!(parts[0].fstype.as_deref(), Some("vfat"));
    }

    #[test]
    fn mounted_usb_partition_is_excluded() {
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert!(!parts.iter().any(|p| p.device == Path::new("/dev/sda1")));
    }

    #[test]
    fn internal_nvme_partition_is_excluded_despite_matching_mountpoint_and_type() {
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert!(!parts
            .iter()
            .any(|p| p.device == Path::new("/dev/nvme0n1p1")));
    }

    #[test]
    fn luks_partition_on_usb_is_excluded() {
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert!(!parts.iter().any(|p| p.device == Path::new("/dev/sdc1")));
    }

    #[test]
    fn lvm_member_partition_on_usb_is_excluded() {
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert!(!parts.iter().any(|p| p.device == Path::new("/dev/sdd1")));
    }

    #[test]
    fn top_level_usb_disk_row_itself_is_not_offered() {
        // Only `type == "part"` rows qualify — the disk row (sdb, type "disk") that
        // parents the one genuinely unmounted partition must not itself appear.
        let parts = parse_lsblk_unmounted_usb_partitions(LSBLK_FIXTURE).unwrap();
        assert!(!parts.iter().any(|p| p.device == Path::new("/dev/sdb")));
    }

    #[test]
    fn malformed_lsblk_output_is_a_typed_parse_error() {
        let err = parse_lsblk_unmounted_usb_partitions("not json").unwrap_err();
        assert!(matches!(err, CoreError::LsblkParseFailed(_)));
    }

    #[test]
    fn empty_blockdevices_list_parses_to_no_partitions() {
        let parts = parse_lsblk_unmounted_usb_partitions(r#"{"blockdevices": []}"#).unwrap();
        assert!(parts.is_empty());
    }

    /// `LSBLK_FIXTURE` above always supplies every key, `null` or not — it never exercises
    /// a key being *absent* outright. Real `lsblk` output omits `children` on every leaf
    /// device (already covered above) but can plausibly omit other optional columns too
    /// (e.g. `size`/`fstype` on a device the kernel hasn't finished probing); this pins
    /// down that an entirely-missing optional key degrades to `None`/`0` rather than
    /// panicking or erroring, matching serde's documented `Option<T>`-defaults-when-absent
    /// behavior rather than merely assuming it holds.
    #[test]
    fn partition_missing_optional_keys_outright_does_not_panic_and_degrades_gracefully() {
        let json = r#"{
           "blockdevices": [
              {
                 "name": "sde", "type": "disk", "tran": "usb", "mountpoint": null,
                 "path": "/dev/sde",
                 "children": [
                    { "name": "sde1", "type": "part", "tran": "usb", "path": "/dev/sde1" }
                 ]
              }
           ]
        }"#;
        let parts = parse_lsblk_unmounted_usb_partitions(json).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].size, 0);
        assert_eq!(parts[0].fstype, None);
    }

    /// A device missing `path` outright can't be turned into a mountable row (there's
    /// nothing to hand `udisksctl mount -b`), so it must be dropped silently rather than
    /// panicking — not surfaced as a row with a fabricated or empty device path.
    #[test]
    fn partition_missing_path_outright_is_silently_skipped() {
        let json = r#"{
           "blockdevices": [
              {
                 "name": "sdf", "type": "disk", "tran": "usb", "mountpoint": null,
                 "children": [
                    { "name": "sdf1", "type": "part", "tran": "usb", "size": 4000000000 }
                 ]
              }
           ]
        }"#;
        let parts = parse_lsblk_unmounted_usb_partitions(json).unwrap();
        assert!(parts.is_empty());
    }

    // --- mount-outcome parsing (pure, fixture-driven) --------------------------------

    #[test]
    fn parses_udisksctl_mount_success_line() {
        let device = Path::new("/dev/sdb1");
        let stdout = "Mounted /dev/sdb1 at /run/media/youri/USBSTICK.\n";
        let mounted = parse_mount_success(stdout, device).expect("expected a parsed success");
        assert_eq!(mounted.device, device);
        assert_eq!(
            mounted.mount_point,
            PathBuf::from("/run/media/youri/USBSTICK")
        );
    }

    #[test]
    fn unrecognized_success_stdout_yields_no_parse() {
        // A change in udisksctl's wording shouldn't silently fabricate a mount point —
        // the caller in mount_partition turns this into an explicit MountError instead.
        assert!(parse_mount_success("Something else entirely\n", Path::new("/dev/sdb1")).is_none());
    }

    #[test]
    fn classifies_device_gone_from_stderr() {
        // Verbatim text observed from a real `udisksctl mount -b <nonexistent>` run.
        let stderr = "Error looking up object for device /dev/sdb1\n";
        let err = classify_mount_failure(Path::new("/dev/sdb1"), stderr);
        assert!(
            matches!(err, MountError::DeviceGone { device } if device == Path::new("/dev/sdb1"))
        );
    }

    #[test]
    fn classifies_not_authorized_from_stderr() {
        let stderr = "Error mounting /dev/sdb1: GDBus.Error:org.freedesktop.UDisks2.Error.NotAuthorized: Not authorized to perform operation\n";
        let err = classify_mount_failure(Path::new("/dev/sdb1"), stderr);
        assert!(
            matches!(err, MountError::NotAuthorized { device } if device == Path::new("/dev/sdb1"))
        );
    }

    #[test]
    fn classifies_unrecognized_stderr_as_other() {
        let stderr = "some future udisksctl wording we don't recognize\n";
        let err = classify_mount_failure(Path::new("/dev/sdb1"), stderr);
        assert!(matches!(err, MountError::Other { device, message }
            if device == Path::new("/dev/sdb1") && message.contains("some future")));
    }
}
