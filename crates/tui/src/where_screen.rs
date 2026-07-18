//! The `linux-backup where` command (#32) — the WHERE screen: an interactive Output-location
//! pick-list wired to the real `core::location` discovery/mount flow. Per ADR-0001 ("tui is
//! a thin view"), every decision about *which locations exist*, *how they're ordered*, *how
//! a device is mounted*, and *whether a chosen location is valid* is delegated to `core`
//! (`discover_disks`, `run_lsblk`, `build_pick_list`, `mount_partition`, `resolve_selection`);
//! this module owns only the screen loop, row rendering, cursor placement, and the inline
//! mount-failure message.
//!
//! ## Why a rebuild-every-iteration loop of `Select` calls
//!
//! The pick-list is not a static menu: "↻ Rescan drives" must re-query real hardware, and
//! mounting an unmounted-USB row changes what `discover_disks`/`run_lsblk` report next time.
//! So, exactly like the WHAT screen (`select_cmd`), the loop rebuilds the whole menu fresh
//! from a *fresh* `core::location` query on every iteration. A plain rescan, a successful
//! mount, and a failed-then-retry all reduce to the same primitive — "loop again, re-query
//! core" — rather than any special-cased in-place row surgery in `tui` (which #28's core
//! tests deliberately left to this layer). After a successful mount the cursor is parked on
//! the row the freshly-discovered drive now occupies, so the just-mounted stick reads as an
//! ordinary selected drive (acceptance criterion 3).

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use inquire::error::InquireError;
use inquire::{Select, Text};
use linux_backup_core::backup_dir_name;
use linux_backup_core::location::{
    self, LocationPickList, LocationRow, LocationSelection, MountError,
};
use linux_backup_core::RunContext;

use crate::system;

/// `linux-backup where`: run the WHERE screen standalone against this account's real drives
/// and print the bound Output location.
///
/// Standalone (ahead of #33 chaining WHAT → encryption → WHERE), there is no Selection yet,
/// so the free-space check runs against a zero-byte Selection — it can only ever pass, which
/// is the honest answer when nothing has been picked to back up. The Backup-dir collision
/// check, by contrast, is meaningful even standalone: it uses the same `backup_dir_name` the
/// real `backup` run will compute.
pub fn run() -> Result<()> {
    let ctx = system::build_run_context()?;

    match run_screen(&ctx, 0)? {
        Some(selection) => {
            println!("Output location bound: {}", selection.location.display());
            println!("Backup dir will be: {}", selection.backup_dir.display());
            if let Some(warning) = selection.warning {
                println!(
                    "Warning: Selection ({}) is close to this location's free space ({}).",
                    format_size(warning.selection_size),
                    format_size(warning.available_space)
                );
            }
        }
        None => println!("No Output location chosen."),
    }

    Ok(())
}

/// The WHERE screen's core loop, extracted so both the standalone `where` subcommand and the
/// chained `backup` wizard (#33) drive the same discovery/mount/validation logic.
///
/// `selection_size` is the running Selection's uncompressed byte size threaded in for the
/// non-blocking free-space warning (story 32) — 0 from the standalone subcommand (no
/// Selection exists yet), the real `SelectionModel::total_size()` from the chained flow.
/// Returns `Ok(None)` when the user cancels with Esc/Ctrl-C.
pub fn run_screen(ctx: &RunContext, selection_size: u64) -> Result<Option<LocationSelection>> {
    let dir_name = backup_dir_name(ctx);
    choose_location(ctx, &dir_name, selection_size)
}

/// Drive the pick-list loop until the user binds a location (`Ok(Some)`) or cancels with
/// Esc/Ctrl-C (`Ok(None)`).
///
/// `selection_size` is the running Selection's uncompressed byte size, used only for the
/// non-blocking free-space warning; `dir_name` is the Backup dir name the collision check
/// refuses against.
fn choose_location(
    ctx: &RunContext,
    dir_name: &str,
    selection_size: u64,
) -> Result<Option<LocationSelection>> {
    // After a successful mount, the freshly-discovered drive should start selected on the
    // next redraw (criterion 3). We remember *where it mounted* rather than a row index,
    // because the rebuilt pick-list re-sorts every row and the index is meaningless across
    // iterations — the mount point is the stable identity to re-find it by.
    let mut park_on_mount_point: Option<PathBuf> = None;

    loop {
        let disks = location::discover_disks();
        // A machine without `lsblk` (or a transient failure) simply has no unmounted-USB
        // rows to offer — never a reason to abort the whole screen.
        let unmounted = location::run_lsblk().unwrap_or_default();
        let pick = location::build_pick_list(&disks, &unmounted, &ctx.home, &ctx.user);
        let rows = location_menu_rows(&pick);

        let starting_cursor = park_on_mount_point
            .as_deref()
            .and_then(|mp| cursor_for_mount_point(&rows, mp))
            .or(pick.preselected)
            .unwrap_or(0);
        park_on_mount_point = None;

        let chosen = match Select::new("WHERE to write the backup:", rows)
            .with_starting_cursor(starting_cursor)
            .prompt()
        {
            Ok(row) => row,
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                return Ok(None)
            }
            Err(other) => return Err(other).context("Output-location prompt failed"),
        };

        match chosen.action {
            RowAction::Bind {
                path,
                available_space,
            } => {
                match location::resolve_selection(&path, dir_name, selection_size, available_space)
                {
                    Ok(selection) => return Ok(Some(selection)),
                    // Story 31/33: an unwritable location or a Backup-dir collision is
                    // refused, shown inline, and the loop re-lists — never a crash/exit.
                    Err(err) => println!("Cannot use {}: {err}", path.display()),
                }
            }
            RowAction::Mount { device } => match location::mount_partition(&device) {
                Ok(mounted) => park_on_mount_point = Some(mounted.mount_point),
                // Stories 35-36: a mount failure is reported inline and the loop re-lists a
                // refreshed pick-list rather than aborting.
                Err(err) => println!("{}", mount_failure_message(&err)),
            },
            // Both just fall through to the next iteration, which re-queries core — that
            // re-query *is* the rescan (criterion 2), with no special-casing needed.
            RowAction::Rescan => {}
            RowAction::Custom => {
                if let Some(selection) = prompt_custom_path(ctx, dir_name, selection_size)? {
                    return Ok(Some(selection));
                }
            }
        }
    }
}

/// Prompt for a Custom path (story 30), expand `~`/`$VARS` and create it via core, then
/// validate it like any other location. On any error (cancel, expansion/create failure,
/// validation refusal) this returns `Ok(None)` so the caller re-lists the pick-list.
fn prompt_custom_path(
    ctx: &RunContext,
    dir_name: &str,
    selection_size: u64,
) -> Result<Option<LocationSelection>> {
    let typed = match Text::new("Custom path (~ and $VARS expanded):").prompt() {
        Ok(text) => text,
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            return Ok(None)
        }
        Err(other) => return Err(other).context("Custom-path prompt failed"),
    };

    let resolved =
        match location::resolve_custom_path(&typed, &ctx.home, |name| std::env::var(name).ok()) {
            Ok(path) => path,
            Err(err) => {
                println!("Cannot use '{typed}': {err}");
                return Ok(None);
            }
        };

    // A Custom path isn't tied to a discovered `DiskInfo`, so its free space is unknown here
    // (`None`) rather than fabricated — the free-space warning simply doesn't fire for it.
    match location::resolve_selection(&resolved, dir_name, selection_size, None) {
        Ok(selection) => Ok(Some(selection)),
        Err(err) => {
            println!("Cannot use {}: {err}", resolved.display());
            Ok(None)
        }
    }
}

/// What choosing a given pick-list row does. Kept alongside the pre-rendered `label` on
/// [`LocationMenuRow`] so `inquire::Select`'s `T: Display` requirement needs no second
/// lookup back into the `LocationPickList` after the user picks a row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAction {
    /// A directly-bindable location: `$HOME` or an already-mounted drive. `available_space`
    /// is `Some` only for a mounted drive (whose `DiskInfo` carries it); `$HOME`'s is
    /// unknown here, so its free-space warning simply doesn't fire.
    Bind {
        path: PathBuf,
        available_space: Option<u64>,
    },
    /// An unmounted USB partition: selecting it drives `core::location::mount_partition`.
    Mount { device: PathBuf },
    /// Re-query core and rebuild the pick-list.
    Rescan,
    /// Prompt for a Custom path.
    Custom,
}

/// One rendered pick-list row: its [`RowAction`] plus display text.
struct LocationMenuRow {
    action: RowAction,
    label: String,
}

impl fmt::Display for LocationMenuRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Render the `core`-built pick-list into `inquire` rows, preserving core's order verbatim
/// (ADR-0001: `tui` never re-orders or filters what `build_pick_list` already decided).
fn location_menu_rows(pick: &LocationPickList) -> Vec<LocationMenuRow> {
    pick.rows
        .iter()
        .map(|row| LocationMenuRow {
            action: row_action(row),
            label: row_label(row),
        })
        .collect()
}

fn row_action(row: &LocationRow) -> RowAction {
    match row {
        LocationRow::Home { path } => RowAction::Bind {
            path: path.clone(),
            available_space: None,
        },
        LocationRow::Mount {
            mount_point,
            available_space,
            ..
        } => RowAction::Bind {
            path: mount_point.clone(),
            available_space: Some(*available_space),
        },
        LocationRow::UnmountedUsbPartition { device, .. } => RowAction::Mount {
            device: device.clone(),
        },
        LocationRow::RescanDrives => RowAction::Rescan,
        LocationRow::CustomPath => RowAction::Custom,
    }
}

/// Render one pick-list row's label. A mounted drive shows free/total space and a `(removable)`
/// tag; an unmounted USB row shows its size, fstype, and the `Enter to mount` affordance
/// story 34 calls for.
fn row_label(row: &LocationRow) -> String {
    match row {
        LocationRow::Home { path } => format!("$HOME  ({})", path.display()),
        LocationRow::Mount {
            mount_point,
            file_system,
            available_space,
            total_space,
            is_removable,
        } => {
            let removable = if *is_removable { "  (removable)" } else { "" };
            format!(
                "{}  [{}]  {} free of {}{}",
                mount_point.display(),
                file_system,
                format_size(*available_space),
                format_size(*total_space),
                removable
            )
        }
        LocationRow::UnmountedUsbPartition {
            device,
            size,
            fstype,
        } => {
            let fs = fstype.as_deref().unwrap_or("unknown fs");
            format!(
                "{}  [{}]  {}  — not mounted (Enter to mount)",
                device.display(),
                fs,
                format_size(*size)
            )
        }
        LocationRow::RescanDrives => "↻ Rescan drives".to_string(),
        LocationRow::CustomPath => "Custom path…".to_string(),
    }
}

/// Index of the rebuilt row that binds `mount_point` — used to park the cursor on a
/// just-mounted drive after re-discovery (criterion 3). `None` if no row binds it (e.g. the
/// drive vanished between mounting and re-discovery), leaving the caller to fall back to the
/// pick-list's own preselection.
fn cursor_for_mount_point(rows: &[LocationMenuRow], mount_point: &Path) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(&row.action, RowAction::Bind { path, .. } if path == mount_point))
}

/// A user-facing one-liner for a failed mount (story 36), classified by `core` into an
/// actionable cause rather than raw `udisksctl` stderr where possible.
fn mount_failure_message(err: &MountError) -> String {
    match err {
        MountError::NotAuthorized { device } => format!(
            "Could not mount {}: not authorized (polkit denied the request).",
            device.display()
        ),
        MountError::DeviceGone { device } => format!(
            "Could not mount {}: the device is gone (unplugged?).",
            device.display()
        ),
        MountError::Other { device, message } => {
            format!("Could not mount {}: {message}", device.display())
        }
    }
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based). Mirrors `select_cmd`'s own
/// size rendering so both wizard screens read sizes the same way; kept local rather than
/// shared to avoid coupling the two screen modules while #31/#32 land in parallel.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    // Disk byte counts stay well under 2^52, so f64 loss is negligible for this human-facing
    // rendering.
    #[allow(clippy::cast_precision_loss)]
    let bytes_f = bytes as f64;
    if bytes_f >= GB {
        format!("{:.2} GB", bytes_f / GB)
    } else if bytes_f >= MB {
        format!("{:.2} MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.2} KB", bytes_f / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linux_backup_core::location::{DiskInfo, UnmountedPartition};

    fn disk(mount_point: &str, removable: bool) -> DiskInfo {
        DiskInfo {
            mount_point: PathBuf::from(mount_point),
            total_space: 2_000_000_000,
            available_space: 1_000_000_000,
            file_system: "ext4".to_string(),
            is_removable: removable,
            is_read_only: false,
        }
    }

    fn unmounted(device: &str) -> UnmountedPartition {
        UnmountedPartition {
            device: PathBuf::from(device),
            size: 8_000_000_000,
            fstype: Some("vfat".to_string()),
        }
    }

    // --- row rendering ---------------------------------------------------------------

    #[test]
    fn home_row_label_names_home_and_its_path() {
        let label = row_label(&LocationRow::Home {
            path: PathBuf::from("/home/youri"),
        });
        assert!(label.contains("$HOME"));
        assert!(label.contains("/home/youri"));
    }

    #[test]
    fn mount_row_label_shows_free_of_total_and_removable_tag() {
        let label = row_label(&LocationRow::Mount {
            mount_point: PathBuf::from("/run/media/youri/USBSTICK"),
            file_system: "vfat".to_string(),
            available_space: 1024 * 1024 * 1024,
            total_space: 2 * 1024 * 1024 * 1024,
            is_removable: true,
        });
        assert!(label.contains("/run/media/youri/USBSTICK"));
        assert!(label.contains("vfat"));
        assert!(label.contains("1.00 GB free of 2.00 GB"));
        assert!(label.contains("(removable)"));
    }

    #[test]
    fn non_removable_mount_row_label_omits_removable_tag() {
        let label = row_label(&LocationRow::Mount {
            mount_point: PathBuf::from("/mnt/data"),
            file_system: "ext4".to_string(),
            available_space: 500,
            total_space: 1000,
            is_removable: false,
        });
        assert!(!label.contains("(removable)"));
    }

    #[test]
    fn unmounted_usb_row_label_shows_enter_to_mount_affordance() {
        let label = row_label(&LocationRow::UnmountedUsbPartition {
            device: PathBuf::from("/dev/sdb1"),
            size: 8_000_000_000,
            fstype: Some("vfat".to_string()),
        });
        assert!(label.contains("/dev/sdb1"));
        assert!(label.contains("vfat"));
        assert!(label.contains("not mounted"));
        assert!(label.contains("Enter to mount"));
    }

    #[test]
    fn unmounted_usb_row_label_tolerates_unknown_fstype() {
        let label = row_label(&LocationRow::UnmountedUsbPartition {
            device: PathBuf::from("/dev/sdb1"),
            size: 4_000_000_000,
            fstype: None,
        });
        assert!(label.contains("unknown fs"));
    }

    #[test]
    fn rescan_and_custom_rows_have_stable_labels() {
        assert!(row_label(&LocationRow::RescanDrives).contains("Rescan"));
        assert!(row_label(&LocationRow::CustomPath).contains("Custom path"));
    }

    // --- row actions -----------------------------------------------------------------

    #[test]
    fn home_and_mount_rows_map_to_bind_actions_carrying_space_only_for_mounts() {
        assert_eq!(
            row_action(&LocationRow::Home {
                path: PathBuf::from("/home/youri"),
            }),
            RowAction::Bind {
                path: PathBuf::from("/home/youri"),
                available_space: None,
            }
        );
        assert_eq!(
            row_action(&LocationRow::Mount {
                mount_point: PathBuf::from("/mnt/data"),
                file_system: "ext4".to_string(),
                available_space: 4_096,
                total_space: 8_192,
                is_removable: false,
            }),
            RowAction::Bind {
                path: PathBuf::from("/mnt/data"),
                available_space: Some(4_096),
            }
        );
    }

    #[test]
    fn unmounted_usb_row_maps_to_a_mount_action_on_its_device() {
        assert_eq!(
            row_action(&LocationRow::UnmountedUsbPartition {
                device: PathBuf::from("/dev/sdb1"),
                size: 8_000_000_000,
                fstype: Some("vfat".to_string()),
            }),
            RowAction::Mount {
                device: PathBuf::from("/dev/sdb1"),
            }
        );
    }

    // --- rescan-picks-up-a-hot-plugged-drive (criterion 2), over injected fixtures ----

    #[test]
    fn a_rescan_after_a_drive_is_plugged_in_surfaces_a_new_bind_row() {
        let home = PathBuf::from("/home/youri");

        // Before the hot-plug: only $HOME plus the trailing Rescan/Custom rows bind.
        let before = location_menu_rows(&location::build_pick_list(&[], &[], &home, "youri"));
        assert!(!before.iter().any(|r| matches!(
            &r.action,
            RowAction::Bind { path, .. } if path == Path::new("/run/media/youri/NEW")
        )));

        // A fresh core query after the drive appears is all "↻ Rescan drives" does; the new
        // drive shows up as an ordinary bind row with no special-casing in this layer.
        let after = location_menu_rows(&location::build_pick_list(
            &[disk("/run/media/youri/NEW", true)],
            &[],
            &home,
            "youri",
        ));
        assert!(after.iter().any(|r| matches!(
            &r.action,
            RowAction::Bind { path, .. } if path == Path::new("/run/media/youri/NEW")
        )));
    }

    // --- cursor lands on the just-mounted drive (criterion 3), over injected fixtures --

    #[test]
    fn cursor_parks_on_the_bind_row_matching_the_new_mount_point() {
        let home = PathBuf::from("/home/youri");
        // Post-mount discovery: the stick that was unmounted now reports as a normal drive.
        let rows = location_menu_rows(&location::build_pick_list(
            &[disk("/run/media/youri/USBSTICK", true)],
            &[],
            &home,
            "youri",
        ));

        let idx = cursor_for_mount_point(&rows, Path::new("/run/media/youri/USBSTICK"))
            .expect("the freshly-mounted drive should have a bind row");
        assert!(matches!(
            &rows[idx].action,
            RowAction::Bind { path, .. } if path == Path::new("/run/media/youri/USBSTICK")
        ));
    }

    #[test]
    fn cursor_lookup_returns_none_when_no_row_matches_the_mount_point() {
        let home = PathBuf::from("/home/youri");
        let rows = location_menu_rows(&location::build_pick_list(&[], &[], &home, "youri"));
        assert_eq!(
            cursor_for_mount_point(&rows, Path::new("/run/media/youri/GONE")),
            None
        );
    }

    #[test]
    fn a_still_unmounted_partition_is_not_a_cursor_park_target() {
        let home = PathBuf::from("/home/youri");
        // An unmounted USB row is a Mount action, not a Bind — so even sharing a path-like
        // identity it must never be picked as the post-mount cursor target.
        let rows = location_menu_rows(&location::build_pick_list(
            &[],
            &[unmounted("/dev/sdb1")],
            &home,
            "youri",
        ));
        assert_eq!(cursor_for_mount_point(&rows, Path::new("/dev/sdb1")), None);
    }

    // --- mount-failure message formatting (story 36) ---------------------------------

    #[test]
    fn not_authorized_mount_failure_message_is_actionable() {
        let msg = mount_failure_message(&MountError::NotAuthorized {
            device: PathBuf::from("/dev/sdb1"),
        });
        assert!(msg.contains("/dev/sdb1"));
        assert!(msg.contains("not authorized"));
    }

    #[test]
    fn device_gone_mount_failure_message_names_the_unplug() {
        let msg = mount_failure_message(&MountError::DeviceGone {
            device: PathBuf::from("/dev/sdb1"),
        });
        assert!(msg.contains("/dev/sdb1"));
        assert!(msg.contains("gone"));
    }

    #[test]
    fn other_mount_failure_message_carries_the_underlying_text() {
        let msg = mount_failure_message(&MountError::Other {
            device: PathBuf::from("/dev/sdb1"),
            message: "some udisksctl detail".to_string(),
        });
        assert!(msg.contains("/dev/sdb1"));
        assert!(msg.contains("some udisksctl detail"));
    }

    // --- size rendering --------------------------------------------------------------

    #[test]
    fn format_size_renders_bytes_kb_mb_gb_thresholds() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }
}
