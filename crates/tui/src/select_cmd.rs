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
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use inquire::error::InquireError;
use inquire::{MultiSelect, Select, Text};
use linux_backup_core::catalog;
use linux_backup_core::crypto;
use linux_backup_core::location;
use linux_backup_core::model::{Entry, SelectionModel};
use linux_backup_core::profile::Profile;
use linux_backup_core::scan::{self, CategoryScan};

use linux_backup_core::RunContext;

use crate::system;

pub fn run() -> Result<()> {
    let ctx = system::build_run_context()?;
    // The standalone `select` subcommand has no Profile to pre-fill from — it always opens on
    // Smart defaults, which is exactly the `None` path the chained wizard also takes on a first
    // run.
    let model = run_screen(&ctx, None)?;

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
///
/// `profile` seeds the initial Selection on a returning run (story 58): `Some` reconstructs it
/// from the saved Profile against this run's fresh scan; `None` opens on Smart defaults exactly
/// as before. Either way the menu itself is always driven by the fresh `scan_taxonomy` result,
/// so a Category that appeared or vanished since last run is reflected correctly.
pub fn run_screen(ctx: &RunContext, profile: Option<&Profile>) -> Result<SelectionModel> {
    let scanned = scan::scan_taxonomy(ctx);
    let mut model = match profile {
        Some(profile) => profile.to_selection_model(&scanned, &ctx.home),
        None => SelectionModel::from_smart_defaults(&scanned),
    };

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
            MenuAction::AddCustomPath => add_custom_path(ctx, &mut model)?,
            MenuAction::Continue => {
                if model.is_empty() {
                    println!("{}", empty_selection_message());
                    continue;
                }
                return Ok(model);
            }
        }
    }
}

/// Story 42's empty-Selection bounce message — the exact wording issue #34 pins down ("check
/// something / add a custom path / Esc to quit"). Extracted from the loop so the wording is
/// unit-testable without driving the interactive screen.
fn empty_selection_message() -> &'static str {
    "Selection is empty — check something, add a Custom path, or Esc to quit."
}

/// Lets the user fine-tune one Category by toggling its Curated-allowlist roots
/// (CONTEXT.md's "Drill-down", #38 / story 13): unconditionally loads every one of `scan`'s
/// Entries into `model` first (`SelectionModel::select_category` is idempotent and preserves
/// any already-deselected root), so the checklist below always has something in the model to
/// reselect/deselect against — including for a Category whose Smart default was OFF and so was
/// never part of `model` at all before this call.
///
/// The toggleable unit is the allowlist *root*, not each expanded file: a single-file root
/// (e.g. `.config/kwinrc`) is its own row, a heavy directory root (`.local/share/icons`, the
/// ~40,000-file icon theme) is one row standing in for its whole subtree — so dropping it is a
/// single toggle rather than unchecking tens of thousands of files. Only roots that actually
/// resolved to Entries on this machine are shown.
fn drill_down(scan: &CategoryScan, model: &mut SelectionModel) -> Result<()> {
    if scan.entries.is_empty() {
        println!("{} has no discovered Entries.", scan.category.name);
        return Ok(());
    }

    model.select_category(scan);

    let roots: Vec<&'static str> = scan
        .category
        .allowlist
        .iter()
        .copied()
        .filter(|root| {
            scan.entries
                .iter()
                .any(|entry| entry.relative_path.starts_with(root))
        })
        .collect();

    if roots.is_empty() {
        println!(
            "{} has no allowlist roots to drill into.",
            scan.category.name
        );
        return Ok(());
    }

    let default_indices: Vec<usize> = roots
        .iter()
        .enumerate()
        .filter(|(_, root)| !model.is_root_deselected(Path::new(root)))
        .map(|(i, _)| i)
        .collect();

    let message = format!(
        "Drill-down: {} — space to toggle an allowlist root, enter to confirm",
        scan.category.name
    );
    let rows: Vec<RootRow> = roots.iter().map(|root| root_row(root, scan)).collect();

    let selected = MultiSelect::new(&message, rows)
        .with_default(&default_indices)
        .prompt()
        .context("Drill-down prompt failed")?;

    let selected_roots: std::collections::BTreeSet<PathBuf> =
        selected.into_iter().map(|row| row.root).collect();

    for root in &roots {
        let root_path = PathBuf::from(root);
        if selected_roots.contains(&root_path) {
            model.reselect_root(&root_path);
        } else {
            model.deselect_root(&root_path);
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
    /// Prompt for a Custom path to add to the Selection (stories 6/8) — the interactive entry
    /// point for the Custom-paths escape hatch, which `scan_taxonomy` can never populate on its
    /// own (its allowlist is empty by design).
    AddCustomPath,
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
        // The Custom-paths Category is the escape hatch (always shown, story 6) but has no
        // scanned Entries to Drill into — its affordance is instead an "add a path" action
        // sitting directly under its row.
        if scan.category.name == catalog::CUSTOM_PATHS.name {
            rows.push(MenuRow {
                action: MenuAction::AddCustomPath,
                label: "    ➕ Add a Custom path".to_string(),
            });
        }
    }
    rows.push(MenuRow {
        action: MenuAction::Continue,
        label: "✔ Continue with the current Selection".to_string(),
    });
    rows
}

/// The "add a Custom path" action (stories 6/8/10/11): prompts for a path, expands `~` and
/// `$VAR`/`${VAR}`, validates it against the root/non-root rule (`catalog::validate_custom_path`),
/// then adds it to the Selection or prints an inline rejection — never a silent failure. A
/// blank entry or an Esc/Ctrl-C cancel is a no-op; the caller re-loops the WHAT screen either
/// way, so an added path shows up in the live footer's running total and Auto-insist state on
/// the next iteration like any other Entry.
fn add_custom_path(ctx: &RunContext, model: &mut SelectionModel) -> Result<()> {
    let entered = match Text::new("Add a Custom path (~ and $VARs are expanded):").prompt() {
        Ok(text) => text,
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            return Ok(());
        }
        Err(other) => return Err(other).context("Custom-path prompt failed"),
    };

    let trimmed = entered.trim();
    if trimmed.is_empty() {
        println!("No path entered — nothing added.");
        return Ok(());
    }

    // Reuse the exact `~`/`$VAR`/`${VAR}` expansion the WHERE screen's Custom output location
    // uses (`location::expand_path`, story 30) so both Custom-path entry points behave
    // identically: env-var lookup happens here in `tui` via the injected closure (ADR-0004
    // keeps the read out of `core`), and an unresolved variable is left literal — so a typo
    // fails validation or the backup read visibly rather than silently vanishing into a wrong
    // path (this is the "handled per the same rules already used elsewhere" the issue names).
    let expanded = location::expand_path(trimmed, &ctx.home, |name| std::env::var(name).ok());

    // Root may add any path; a non-root run is constrained to under $HOME. The rejection
    // message is `CoreError::CustomPathOutsideHome`'s own Display, so it names both the path
    // and $HOME (stories 10/11).
    match catalog::validate_custom_path(&expanded, &ctx.home, ctx.privilege) {
        Ok(()) => {
            model.add_custom_path(&expanded, &ctx.home);
            println!("Added Custom path: {}", expanded.display());
        }
        Err(err) => println!("Rejected: {err}"),
    }

    Ok(())
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

/// One Drill-down root row: the allowlist root (for diffing the `MultiSelect` result back
/// against `roots` in [`drill_down`]) alongside its pre-rendered display text.
struct RootRow {
    root: PathBuf,
    label: String,
}

impl fmt::Display for RootRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

fn root_row(root: &str, scan: &CategoryScan) -> RootRow {
    RootRow {
        root: PathBuf::from(root),
        label: root_row_label(root, scan),
    }
}

/// Renders one Drill-down root row: the root path, 🔒 if any Entry under it is Sensitive, and
/// the count + summed on-disk size of the subtree it stands for — so a user weighs a whole
/// directory Entry (e.g. the ~226MB icon theme) before dropping it in one toggle.
/// Checked/unchecked state itself is `MultiSelect`'s own rendering (driven by `with_default`),
/// not encoded in this label.
fn root_row_label(root: &str, scan: &CategoryScan) -> String {
    let under: Vec<&Entry> = scan
        .entries
        .iter()
        .filter(|entry| entry.relative_path.starts_with(root))
        .collect();
    let count = under.len();
    let size: u64 = under
        .iter()
        .filter_map(|entry| std::fs::metadata(&entry.absolute_path).ok())
        .map(|m| m.len())
        .sum();
    let lock = if under.iter().any(|entry| entry.sensitive) {
        " 🔒"
    } else {
        ""
    };
    let files = if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    };
    format!("{root}{lock} — {files}, {}", format_size(size))
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based), shared by Category and Entry
/// rows and the footer so every size in the screen renders consistently.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    // Byte counts stay well under 2^52, so the f64 mantissa loses nothing for this display.
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
    use linux_backup_core::catalog;
    use linux_backup_core::model::{EntryRoot, EntryType};
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
            entry_type: EntryType::File,
            category,
            sensitive: false,
        }
    }

    #[test]
    fn empty_selection_message_matches_the_specified_bounce_wording() {
        let message = empty_selection_message();
        assert!(message.contains("check something"));
        assert!(message.contains("Custom path"));
        assert!(message.contains("Esc to quit"));
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
    fn root_row_label_sums_the_subtree_and_locks_when_any_entry_is_sensitive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Two files under one heavy directory root — the row must report the whole subtree
        // (2 files, 300 B), not a single Entry, since the root is the toggleable unit.
        let icon_a = entry_with_size(
            tmp.path(),
            catalog::KDE_PLASMA.name,
            ".local/share/icons/a.svg",
            100,
        );
        let icon_b = entry_with_size(
            tmp.path(),
            catalog::KDE_PLASMA.name,
            ".local/share/icons/b.svg",
            200,
        );
        let scan = CategoryScan {
            category: catalog::KDE_PLASMA,
            entries: vec![icon_a, icon_b],
        };

        let label = root_row_label(".local/share/icons", &scan);

        assert!(label.starts_with(".local/share/icons"));
        assert!(label.contains("2 files"));
        assert!(label.contains("300 B"));
        // Nothing under this root is Sensitive, so no lock.
        assert!(!label.contains('🔒'));
    }

    #[test]
    fn root_row_label_marks_a_single_file_root_and_flags_sensitivity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let key = entry_with_size(tmp.path(), catalog::SSH.name, ".ssh/id_ed25519", 256);
        let key = Entry {
            sensitive: true,
            ..key
        };
        let scan = CategoryScan {
            category: catalog::SSH,
            entries: vec![key],
        };

        let label = root_row_label(".ssh/id_ed25519", &scan);

        assert!(label.contains("1 file"));
        assert!(label.contains("256 B"));
        assert!(label.contains('🔒'));
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

        // Shell: Toggle + Drill (has entries). Custom paths: Toggle (always_shown but empty,
        // story 6 — nothing to Drill into) followed by its "Add a Custom path" action. Plus
        // the trailing Continue row.
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0].action, MenuAction::Toggle(0)));
        assert!(matches!(rows[1].action, MenuAction::Drill(0)));
        assert!(matches!(rows[2].action, MenuAction::Toggle(1)));
        assert!(matches!(rows[3].action, MenuAction::AddCustomPath));
        assert!(rows[3].label.contains("Add a Custom path"));
        assert!(matches!(rows[4].action, MenuAction::Continue));
        assert!(rows[4].label.contains("Continue"));
    }

    #[test]
    fn menu_rows_offers_add_custom_path_even_on_an_empty_account() {
        // Story 62: an account with no discoverable Categories still scans Custom paths (it's
        // exempt from Hide-empty), so the "add a Custom path" action is reachable — the only
        // way such an account can build a non-empty Selection and complete a backup.
        let scanned = vec![CategoryScan {
            category: catalog::CUSTOM_PATHS,
            entries: Vec::new(),
        }];
        let model = SelectionModel::from_smart_defaults(&scanned);

        let rows = menu_rows(&scanned, &model);

        assert!(rows
            .iter()
            .any(|row| matches!(row.action, MenuAction::AddCustomPath)));
    }
}
