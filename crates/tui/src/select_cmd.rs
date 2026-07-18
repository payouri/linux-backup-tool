//! The `linux-backup select` command (#30) — the WHAT screen: an interactive Category
//! checklist with 🔒/size annotations, Drill-down into individual Entries, and a live
//! footer of running Selection size + Auto-insist state. Per ADR-0001 ("tui is a thin
//! view"), every decision about *what's selected* is delegated to `core` (`scan::scan_taxonomy`,
//! `SelectionModel`, `crypto::evaluate`); this module owns only the screen loop, rendering,
//! and cursor navigation.
//!
//! ## Why a loop of `Select`/`MultiSelect` calls, not one `MultiSelect` call
//!
//! `inquire` 0.9's `MultiSelect` renders any `T: Display` as a checkbox row with its own
//! fixed keybindings (space to toggle, → / ← to select-all/none, Enter to submit-and-exit) —
//! there's no hook for a per-row alternate action like "press this key on this specific row
//! to descend a level." Drill-down needs exactly that: a per-Category action distinct from
//! toggling its checkbox. So the WHAT screen is a loop of `inquire::Select` calls over a
//! menu that is rebuilt fresh every iteration from the current `scan_taxonomy` result and
//! `SelectionModel` state — one row per Category to toggle its checkbox on/off, plus a
//! second "→ Drill-down" row per non-empty Category, plus a trailing "Continue" row to
//! finish. `MultiSelect` is still used, but only where it's actually the right shape: the
//! per-Category Drill-down screen itself is a single, one-shot checklist of that Category's
//! Entries, submitted once. Rebuilding the menu (and its footer) every loop iteration is
//! what makes the footer "live" (recomputed and shown after every toggle) without any
//! background-updating widget.

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use inquire::{MultiSelect, Select};
use linux_backup_core::crypto;
use linux_backup_core::model::{Entry, SelectionModel};
use linux_backup_core::scan::{self, CategoryScan};

use linux_backup_core::RunContext;

use crate::system;

pub fn run() -> Result<()> {
    let ctx = system::build_run_context()?;
    let model = run_screen(&ctx)?;

    println!("Final selection:");
    println!("{}", footer_text(&model));
    for entry in model.entries() {
        println!("  - [{}] {}", entry.category, entry.relative_path.display());
    }

    Ok(())
}

/// The WHAT screen's core loop, extracted so both the standalone `select` subcommand and the
/// chained `backup` wizard (#33) drive the same screen logic rather than duplicating it.
///
/// Returns a guaranteed-non-empty [`SelectionModel`]: story 42's empty-Selection bounce is
/// enforced here — pressing Continue with nothing selected reprints the bounce message and
/// re-loops rather than returning an empty Selection for a caller to have to re-check.
pub fn run_screen(ctx: &RunContext) -> Result<SelectionModel> {
    let scanned = scan::scan_taxonomy(ctx);
    let mut model = SelectionModel::from_smart_defaults(&scanned);

    loop {
        println!();
        println!("{}", footer_text(&model));

        let rows = menu_rows(&scanned, &model);
        let chosen = Select::new(
            "WHAT to back up — pick a Category to toggle, drill into its Entries, or continue:",
            rows,
        )
        .prompt()
        .context("Category selection prompt failed")?;

        match chosen.action {
            MenuAction::Toggle(index) => {
                let scan = &scanned[index];
                if active_count(scan, &model) > 0 {
                    model.deselect_category(scan.category.name);
                } else {
                    model.select_category(scan);
                }
            }
            MenuAction::Drill(index) => drill_down(&scanned[index], &mut model)?,
            MenuAction::Continue => {
                if model.is_empty() {
                    println!(
                        "Selection is empty — check something, add a Custom path, or Esc to quit."
                    );
                    continue;
                }
                return Ok(model);
            }
        }
    }
}

/// Lets the user fine-tune one Category's individual Entries (CONTEXT.md's "Drill-down"):
/// unconditionally loads every one of `scan`'s Entries into `model` first
/// (`SelectionModel::select_category` is idempotent and preserves any already-withheld
/// Entry), so the per-Entry checklist below always has something in the model to
/// reselect/deselect against — including for a Category whose Smart default was OFF and so
/// was never part of `model` at all before this call.
fn drill_down(scan: &CategoryScan, model: &mut SelectionModel) -> Result<()> {
    if scan.entries.is_empty() {
        println!("{} has no discovered Entries.", scan.category.name);
        return Ok(());
    }

    model.select_category(scan);

    let default_indices: Vec<usize> = scan
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !model.is_deselected(&e.relative_path))
        .map(|(i, _)| i)
        .collect();

    let message = format!(
        "Drill-down: {} — space to toggle an Entry, enter to confirm",
        scan.category.name
    );
    let rows: Vec<EntryRow> = scan.entries.iter().map(entry_row).collect();

    let selected = MultiSelect::new(&message, rows)
        .with_default(&default_indices)
        .prompt()
        .context("Drill-down prompt failed")?;

    let selected_paths: std::collections::BTreeSet<PathBuf> =
        selected.into_iter().map(|row| row.relative_path).collect();

    for entry in &scan.entries {
        if selected_paths.contains(&entry.relative_path) {
            model.reselect_entry(&entry.relative_path);
        } else {
            model.deselect_entry(&entry.relative_path);
        }
    }

    Ok(())
}

/// One row of the top-level Category menu: what happens if it's chosen (`action`) alongside
/// its pre-rendered display text (`label`) — kept together so `inquire::Select`'s `T:
/// Display` requirement doesn't need a second lookup back into `scanned`/`model` after the
/// user picks a row.
struct MenuRow {
    action: MenuAction,
    label: String,
}

impl fmt::Display for MenuRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Copy)]
enum MenuAction {
    /// Toggle the Category at this index in `scanned` on/off as a whole.
    Toggle(usize),
    /// Drill into the Category at this index in `scanned`.
    Drill(usize),
    /// Finish the WHAT screen with whatever is currently selected.
    Continue,
}

/// Builds the top-level menu fresh from the current `scanned`/`model` state: one Toggle row
/// per Category, a Drill row after it when the Category has any discovered Entries, and a
/// trailing Continue row.
fn menu_rows(scanned: &[CategoryScan], model: &SelectionModel) -> Vec<MenuRow> {
    let mut rows = Vec::with_capacity(scanned.len() * 2 + 1);
    for (index, scan) in scanned.iter().enumerate() {
        rows.push(MenuRow {
            action: MenuAction::Toggle(index),
            label: category_row_label(scan, model),
        });
        if !scan.entries.is_empty() {
            rows.push(MenuRow {
                action: MenuAction::Drill(index),
                label: format!(
                    "    → Drill-down: {} ({} entries)",
                    scan.category.name,
                    scan.entries.len()
                ),
            });
        }
    }
    rows.push(MenuRow {
        action: MenuAction::Continue,
        label: "✔ Continue with the current Selection".to_string(),
    });
    rows
}

/// How many of `scan`'s discovered Entries are currently active in `model` — the numerator
/// of a Category row's "(m/n entries)" partial-selection hint, and the source of truth for
/// whether a Category counts as checked at all (a Category never toggled on has zero
/// Entries in `model` at all, active or withheld — `model.is_deselected` alone can't tell
/// that apart from "fully checked," so this counts against `model.entries()` directly).
fn active_count(scan: &CategoryScan, model: &SelectionModel) -> usize {
    let active_paths: std::collections::BTreeSet<&std::path::Path> = model
        .entries()
        .iter()
        .map(|e| e.relative_path.as_path())
        .collect();
    scan.entries
        .iter()
        .filter(|e| active_paths.contains(e.relative_path.as_path()))
        .count()
}

/// Renders one Category row: checkbox glyph, 🔒 if Sensitive, computed size, and a
/// partial-selection hint when some but not all of its Entries are active.
fn category_row_label(scan: &CategoryScan, model: &SelectionModel) -> String {
    let total = scan.entries.len();
    let active = active_count(scan, model);
    let glyph = match (active, total) {
        (0, _) => "[ ]",
        (a, t) if a == t => "[x]",
        _ => "[~]",
    };
    let lock = if scan.is_sensitive() { " 🔒" } else { "" };
    let size = format_size(scan.total_size());

    let mut label = format!("{glyph} {}{lock} — {size}", scan.category.name);
    // Only the partial ([~]) case gets the "(m/n entries)" suffix — a fully off Category
    // (active == 0) is already unambiguous from its "[ ]" glyph alone, so pairing it with
    // "(0/n entries)" would read like a partial-selection hint for a Category that was
    // never checked in the first place.
    if total > 0 && active != total && active != 0 {
        label.push_str(&format!(" ({active}/{total} entries)"));
    }
    label
}

/// Renders the live footer (story 9/18): running Selection size and current Auto-insist
/// state, naming which Sensitive Categories triggered it.
fn footer_text(model: &SelectionModel) -> String {
    let size = format_size(model.total_size());
    let policy = crypto::evaluate(model);
    let encryption = if policy.encryption_required {
        format!(
            "Encryption: REQUIRED (Auto-insist — {})",
            policy.sensitive_categories.join(", ")
        )
    } else {
        "Encryption: not required".to_string()
    };
    format!("Running total: {size} · {encryption}")
}

/// One Drill-down Entry row: the relative path (for diffing the `MultiSelect` result back
/// against `scan.entries` in [`drill_down`]) alongside its pre-rendered display text.
struct EntryRow {
    relative_path: PathBuf,
    label: String,
}

impl fmt::Display for EntryRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

fn entry_row(entry: &Entry) -> EntryRow {
    EntryRow {
        relative_path: entry.relative_path.clone(),
        label: entry_row_label(entry),
    }
}

/// Renders one Drill-down Entry row: relative path, 🔒 if Sensitive, and on-disk size.
/// Checked/unchecked state itself is `MultiSelect`'s own rendering (driven by
/// `with_default`), not encoded in this label.
fn entry_row_label(entry: &Entry) -> String {
    let lock = if entry.sensitive { " 🔒" } else { "" };
    let size = std::fs::metadata(&entry.absolute_path)
        .map(|m| format_size(m.len()))
        .unwrap_or_else(|_| "unknown size".to_string());
    format!("{}{lock} — {size}", entry.relative_path.display())
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based), shared by Category and Entry
/// rows and the footer so every size in the screen renders consistently.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

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
    use linux_backup_core::catalog;
    use linux_backup_core::model::EntryRoot;
    use std::path::Path;

    /// Builds a real on-disk Entry of `len` bytes under `tmp`, at `relative_path`, since
    /// `CategoryScan::total_size`/`entry_row_label` read real file sizes via `fs::metadata`
    /// (same pattern `core::model`'s own tests use for the same reason).
    fn entry_with_size(tmp: &Path, category: &'static str, relative_path: &str, len: u64) -> Entry {
        let absolute_path = tmp.join(relative_path);
        std::fs::create_dir_all(absolute_path.parent().unwrap()).expect("create parent dir");
        let file = std::fs::File::create(&absolute_path).expect("create file");
        file.set_len(len).expect("set_len");
        Entry {
            absolute_path,
            relative_path: PathBuf::from(relative_path),
            root: EntryRoot::Home,
            category,
            sensitive: false,
        }
    }

    #[test]
    fn format_size_renders_bytes_kb_mb_gb_thresholds() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_size(1024 * 1024 * 1024 * 2), "2.00 GB");
    }

    #[test]
    fn active_count_is_zero_for_a_category_never_toggled_on() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::USER_DATA.name, "Documents/a.txt", 10);
        let scan = CategoryScan {
            category: catalog::USER_DATA,
            entries: vec![entry],
        };
        // Smart default OFF: `from_smart_defaults` never includes User data at all.
        let model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));

        assert_eq!(active_count(&scan, &model), 0);
    }

    #[test]
    fn active_count_is_full_when_a_smart_default_on_category_is_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::SHELL.name, "aliases.zsh", 10);
        let scan = CategoryScan {
            category: catalog::SHELL,
            entries: vec![entry],
        };
        let model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));

        assert_eq!(active_count(&scan, &model), 1);
    }

    #[test]
    fn active_count_reflects_a_drill_down_deselection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), catalog::KDE_PLASMA.name, ".config/kwinrc", 10);
        let icon = entry_with_size(
            tmp.path(),
            catalog::KDE_PLASMA.name,
            ".local/share/icons/heavy.svg",
            200,
        );
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![kwinrc, icon],
        };
        let mut model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));
        model.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        assert_eq!(active_count(&scan, &model), 1);
    }

    #[test]
    fn category_row_label_shows_unchecked_glyph_and_no_lock_for_an_off_non_sensitive_category() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::USER_DATA.name, "Documents/a.txt", 1024);
        let scan = CategoryScan {
            category: catalog::USER_DATA,
            entries: vec![entry],
        };
        let model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));

        let label = category_row_label(&scan, &model);

        assert!(label.starts_with("[ ] User data"));
        assert!(!label.contains('🔒'));
        assert!(label.contains("1.00 KB"));
        // A fully off Category is already unambiguous from its "[ ]" glyph — it must not
        // also carry a "(0/n entries)" suffix, which would misread as a partial-selection
        // hint (reserved for the "[~]" case) on a Category that was never checked at all.
        assert!(!label.contains("entries)"));
    }

    #[test]
    fn category_row_label_omits_the_entry_count_for_a_category_toggled_fully_off() {
        // Reproduces manual verification during #30's review: toggling an on-by-default,
        // multi-Entry Category off (`deselect_category`) must not leave a stale
        // "(0/n entries)" suffix behind — that reads as a partial-selection hint even though
        // the Category's "[ ]" glyph already says nothing is selected.
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), catalog::KDE_PLASMA.name, ".config/kwinrc", 10);
        let icon = entry_with_size(
            tmp.path(),
            catalog::KDE_PLASMA.name,
            ".local/share/icons/heavy.svg",
            200,
        );
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![kwinrc, icon],
        };
        let mut model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));
        model.deselect_category(catalog::KDE_PLASMA.name);

        let label = category_row_label(&scan, &model);

        assert!(label.starts_with("[ ] KDE Plasma"));
        assert!(!label.contains("entries)"));
    }

    #[test]
    fn category_row_label_shows_checked_glyph_and_lock_for_a_fully_selected_sensitive_category() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::SSH.name, "id_ed25519", 512);
        let entry = Entry {
            sensitive: true,
            ..entry
        };
        let scan = CategoryScan {
            category: catalog::SSH,
            entries: vec![entry],
        };
        let model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));

        let label = category_row_label(&scan, &model);

        assert!(label.starts_with("[x] SSH 🔒"));
        assert!(!label.contains("entries)"));
    }

    #[test]
    fn category_row_label_shows_partial_glyph_and_count_after_a_drill_down_deselection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kwinrc = entry_with_size(tmp.path(), catalog::KDE_PLASMA.name, ".config/kwinrc", 10);
        let icon = entry_with_size(
            tmp.path(),
            catalog::KDE_PLASMA.name,
            ".local/share/icons/heavy.svg",
            200,
        );
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![kwinrc, icon],
        };
        let mut model = SelectionModel::from_smart_defaults(std::slice::from_ref(&scan));
        model.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        let label = category_row_label(&scan, &model);

        assert!(label.starts_with("[~] KDE Plasma"));
        assert!(label.contains("(1/2 entries)"));
    }

    #[test]
    fn footer_text_reports_no_encryption_required_with_no_sensitive_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::SHELL.name, "aliases.zsh", 100);
        let model = SelectionModel::new(vec![entry]);

        let footer = footer_text(&model);

        assert!(footer.contains("100 B"));
        assert!(footer.contains("Encryption: not required"));
    }

    #[test]
    fn footer_text_names_the_sensitive_category_that_triggered_auto_insist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entry = entry_with_size(tmp.path(), catalog::SSH.name, "id_ed25519", 100);
        let entry = Entry {
            sensitive: true,
            ..entry
        };
        let model = SelectionModel::new(vec![entry]);

        let footer = footer_text(&model);

        assert!(footer.contains("Encryption: REQUIRED"));
        assert!(footer.contains("SSH"));
    }

    #[test]
    fn entry_row_label_includes_lock_icon_only_when_sensitive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sensitive_entry = entry_with_size(tmp.path(), catalog::SSH.name, "id_ed25519", 256);
        let sensitive_entry = Entry {
            sensitive: true,
            ..sensitive_entry
        };
        let plain_entry = entry_with_size(tmp.path(), catalog::SHELL.name, "aliases.zsh", 256);

        assert!(entry_row_label(&sensitive_entry).contains('🔒'));
        assert!(!entry_row_label(&plain_entry).contains('🔒'));
        assert!(entry_row_label(&plain_entry).contains("256 B"));
    }

    #[test]
    fn menu_rows_include_a_drill_row_only_for_categories_with_entries_and_a_trailing_continue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shell_entry = entry_with_size(tmp.path(), catalog::SHELL.name, "aliases.zsh", 10);
        let scanned = vec![
            CategoryScan {
                category: catalog::SHELL,
                entries: vec![shell_entry],
            },
            CategoryScan {
                category: catalog::CUSTOM_PATHS,
                entries: Vec::new(),
            },
        ];
        let model = SelectionModel::from_smart_defaults(&scanned);

        let rows = menu_rows(&scanned, &model);

        // Shell: Toggle + Drill (has entries). Custom paths: Toggle only (always_shown but
        // empty, story 6 — nothing to Drill into yet). Plus the trailing Continue row.
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0].action, MenuAction::Toggle(0)));
        assert!(matches!(rows[1].action, MenuAction::Drill(0)));
        assert!(matches!(rows[2].action, MenuAction::Toggle(1)));
        assert!(matches!(rows[3].action, MenuAction::Continue));
        assert!(rows[3].label.contains("Continue"));
    }
}
