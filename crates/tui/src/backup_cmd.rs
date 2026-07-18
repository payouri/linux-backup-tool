//! The `linux-backup backup` command (#33) — the full forward-only wizard that chains the
//! three standalone screens (#30/#31/#32) into the real end-to-end backup flow:
//! WHAT → [encryption, if Auto-insist triggers] → WHERE → REVIEW → collision-lock → RUN.
//!
//! Per ADR-0001 ("tui is a thin view over core"), this module owns only the chaining, the
//! REVIEW rendering, and the progress-bar adapter; every decision — what's selected, what
//! encryption is required, which locations exist, and the backup itself — is delegated to
//! `core` (`select_cmd`/`encryption_cmd`/`where_screen` in turn delegate to `scan`/`crypto`/
//! `location`), and RUN calls straight into `core::backup`.
//!
//! ## Ordering the acceptance criteria pin down
//!
//! REVIEW ([`review_text`]) summarizes the bound Selection, Output location, and encryption
//! state and is printed — followed by a forward-only confirm — *before* [`run_backup`] is
//! ever reached, so nothing is written until the user confirms. The collision-lock
//! ([`precheck_collision`]) then re-checks the computed Backup dir right before RUN, so a
//! collision is surfaced here rather than as `core`'s `BackupDirExists` mid-run. The RUN step
//! is the only place anything is written, and it produces a Backup dir byte-for-content
//! identical to what the equivalent `core`-level round-trip test produces because it calls the
//! exact same `core::backup`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use inquire::error::InquireError;
use inquire::Confirm;
use linux_backup_core::archive::Encryption;
use linux_backup_core::crypto;
use linux_backup_core::location::LocationSelection;
use linux_backup_core::model::SelectionModel;
use linux_backup_core::packages::SystemCommandRunner;
use linux_backup_core::progress::{Phase, Progress};
use linux_backup_core::{backup, BackupOutcome, CoreError, RunContext};

use crate::{encryption_cmd, select_cmd, system, where_screen};

pub fn run() -> Result<()> {
    let mut ctx = system::build_run_context()?;

    // WHAT: a guaranteed-non-empty Selection (the empty-Selection bounce lives inside the
    // screen loop — story 42).
    let selection = select_cmd::run_screen(&ctx)?;

    // Encryption: a no-op returning `Encryption::None` unless a Sensitive Selection triggers
    // Auto-insist (story 17/18/19).
    let outcome = encryption_cmd::run(&selection)?;

    // WHERE: now that a Selection exists, thread its real size in for the free-space warning
    // (#32 left this a documented `0` placeholder standalone).
    let location = match where_screen::run_screen(&ctx, selection.total_size())? {
        Some(location) => location,
        None => {
            println!("No Output location chosen — backup aborted. Nothing was written.");
            return Ok(());
        }
    };

    // `core::backup` computes the Backup dir under `ctx.output_location`; `build_run_context`
    // defaults that to `$HOME`, so point it at the location the WHERE screen actually bound.
    ctx.output_location = location.location.clone();

    // REVIEW: everything chosen, summarized before anything is written.
    println!();
    println!(
        "{}",
        review_text(
            &selection,
            &location,
            &outcome.encryption,
            outcome.auto_insist_disabled
        )
    );
    println!();
    if !confirm_proceed()? {
        println!("Backup aborted at review — nothing was written.");
        return Ok(());
    }

    // Collision-lock (story 33): caught before RUN, not mid-run.
    if let CollisionCheck::Collision(message) = precheck_collision(&location.backup_dir) {
        println!("{message}");
        return Ok(());
    }

    run_backup(&selection, &ctx, &outcome.encryption)
}

/// The forward-only REVIEW confirmation. Esc/Ctrl-C aborts (returns `false`) rather than
/// erroring — a cancel at the last gate is a deliberate "don't proceed", not a failure.
fn confirm_proceed() -> Result<bool> {
    match Confirm::new("Proceed with the backup? (nothing has been written yet)")
        .with_default(true)
        .prompt()
    {
        Ok(proceed) => Ok(proceed),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(false),
        Err(other) => Err(other).context("review confirmation prompt failed"),
    }
}

/// The RUN step: call `core::backup` with a real progress adapter driving the per-phase bar
/// (story 37), then print the Backup dir + summary on success or surface `core`'s typed error
/// cleanly on failure.
fn run_backup(selection: &SelectionModel, ctx: &RunContext, encryption: &Encryption) -> Result<()> {
    println!();
    let mut progress = BarProgress::new(selection.entries().len());
    let result = backup(
        selection,
        ctx,
        encryption,
        &SystemCommandRunner,
        &mut progress,
    );
    // Clear the bar before printing either outcome so the summary/error isn't tangled up with
    // a half-drawn progress line.
    progress.finish();

    let outcome = result.map_err(describe_backup_error)?;
    println!();
    println!("{}", success_summary(&outcome, encryption));
    Ok(())
}

/// Wraps a `core::backup` failure with the ADR-0002 guarantee a user most needs to hear: a
/// failed backup never leaves a partial Backup dir behind (any error after the dir is created
/// deletes it again), so there is nothing to clean up by hand.
fn describe_backup_error(err: CoreError) -> anyhow::Error {
    anyhow::Error::new(err)
        .context("backup run failed — no partial Backup dir was left behind (fail-fast, ADR-0002)")
}

/// The result of the pre-RUN collision-lock: proceed, or stop with a user-facing message.
#[derive(Debug, PartialEq, Eq)]
enum CollisionCheck {
    Clear,
    Collision(String),
}

/// Re-check story 33's collision unit (the Backup dir) right before RUN. The WHERE screen
/// already validated it once via `location::resolve_selection`, but a collision could appear
/// in the window between WHERE and RUN — catching it here keeps `core::backup`'s own
/// `BackupDirExists` refusal from firing mid-run.
fn precheck_collision(backup_dir: &Path) -> CollisionCheck {
    if backup_dir.exists() {
        CollisionCheck::Collision(format!(
            "A Backup dir already exists at {} — it appeared since the WHERE screen validated \
             the location. Nothing was written; re-run and pick another Output location.",
            backup_dir.display()
        ))
    } else {
        CollisionCheck::Clear
    }
}

/// The REVIEW summary (an acceptance criterion: correctly summarizes everything chosen before
/// anything is written). Pure so it's unit-tested without driving the interactive screens.
fn review_text(
    selection: &SelectionModel,
    location: &LocationSelection,
    encryption: &Encryption,
    auto_insist_disabled: bool,
) -> String {
    let breakdown = category_breakdown(selection);

    let mut lines = vec![
        "=== REVIEW — nothing is written until you confirm ===".to_string(),
        format!(
            "Selection: {} Entries across {} Categories, {}",
            selection.entries().len(),
            breakdown.len(),
            format_size(selection.total_size())
        ),
    ];
    for (category, count) in &breakdown {
        lines.push(format!(
            "  - {category}: {count} {}",
            if *count == 1 { "Entry" } else { "Entries" }
        ));
    }
    lines.push(format!("Output location: {}", location.location.display()));
    lines.push(format!(
        "Backup dir:      {}",
        location.backup_dir.display()
    ));
    lines.push(format!(
        "Encryption:      {}",
        encryption_review_line(selection, encryption, auto_insist_disabled)
    ));
    if let Some(warning) = location.warning {
        lines.push(format!(
            "Warning: Selection ({}) is close to this location's free space ({}).",
            format_size(warning.selection_size),
            format_size(warning.available_space)
        ));
    }
    lines.join("\n")
}

/// The REVIEW screen's encryption line. An encrypted run says so; a plaintext run reached by
/// deliberately disabling Auto-insist (`auto_insist_disabled`, straight from the encryption
/// screen's [`EncryptionOutcome`]) names the exposed Sensitive Categories (from
/// `crypto::evaluate`, so the names can't drift from what that screen disclosed); an ordinary
/// plaintext run with nothing Sensitive says that plainly.
fn encryption_review_line(
    selection: &SelectionModel,
    encryption: &Encryption,
    auto_insist_disabled: bool,
) -> String {
    match encryption {
        Encryption::Passphrase(_) => {
            "encrypted with a passphrase (Archive + Manifest both .age)".to_string()
        }
        Encryption::None if auto_insist_disabled => {
            let policy = crypto::evaluate(selection);
            format!(
                "PLAINTEXT — Auto-insist disabled; exposed Sensitive Categories: {}",
                policy.sensitive_categories.join(", ")
            )
        }
        Encryption::None => "plaintext (no Sensitive content in this Selection)".to_string(),
    }
}

/// Per-Category Entry counts for the REVIEW summary, sorted by Category name (via `BTreeMap`)
/// so the summary is deterministic rather than dependent on scan order.
fn category_breakdown(selection: &SelectionModel) -> Vec<(&'static str, usize)> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for entry in selection.entries() {
        *counts.entry(entry.category).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

/// The post-RUN success summary: Backup dir, encryption state, Vanished Entries (calling out
/// any Sensitive ones by name — story 40), and captured per-manager Package-list counts. Pure
/// so it's unit-tested without a real backup run.
fn success_summary(outcome: &BackupOutcome, encryption: &Encryption) -> String {
    let manifest = &outcome.manifest;
    let mut lines = vec![
        format!("Backup complete: {}", outcome.backup_dir.display()),
        format!("Entries recorded: {}", manifest.entries.len()),
        format!(
            "Encryption: {}",
            if encryption.is_encrypted() {
                "encrypted (.age Archive + Manifest)"
            } else {
                "plaintext"
            }
        ),
    ];

    if manifest.vanished.is_empty() {
        lines.push("Vanished Entries: none.".to_string());
    } else {
        lines.push(format!(
            "Vanished Entries (skipped as benign): {}",
            manifest.vanished.len()
        ));
        let sensitive = manifest.vanished_sensitive_names();
        if !sensitive.is_empty() {
            lines.push(format!(
                "  Sensitive Vanished Entries — check these disappeared expectedly: {}",
                sensitive.join(", ")
            ));
        }
    }

    if manifest.packages.is_empty() {
        lines.push("Package lists: none captured.".to_string());
    } else {
        lines.push("Package lists captured:".to_string());
        for (manager, packages) in &manifest.packages {
            lines.push(format!("  - {manager}: {} package(s)", packages.len()));
        }
    }

    lines.join("\n")
}

/// The `crates/tui`-side [`Progress`] implementation: adapts `core`'s `on_phase`/`on_entry`
/// callbacks to an `indicatif` progress bar (story 37's live per-phase progress).
struct BarProgress {
    bar: ProgressBar,
}

impl BarProgress {
    fn new(entry_count: usize) -> Self {
        let bar = ProgressBar::new(entry_count as u64);
        let style = ProgressStyle::with_template("{spinner} {msg} [{bar:30}] {pos}/{len}")
            // The template is a compile-time constant, but production code can't `unwrap`; fall
            // back to indicatif's built-in bar style rather than risk a panic on a typo.
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> ");
        bar.set_style(style);
        // A steady tick animates the spinner even while a single large Entry is being read, so
        // the bar reads as alive during a long phase rather than looking hung.
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

impl Progress for BarProgress {
    fn on_phase(&mut self, phase: Phase) {
        self.bar.set_message(phase_label(phase));
    }

    fn on_entry(&mut self, done: usize, total: usize) {
        if self.bar.length() != Some(total as u64) {
            self.bar.set_length(total as u64);
        }
        self.bar.set_position(done as u64);
    }
}

/// The human-facing label for each backup [`Phase`]. Extracted (rather than inlined in
/// `on_phase`) so the mapping is unit-testable without a running progress bar.
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Scanning => "Reading Entries",
        Phase::WritingArchive => "Writing Archive",
        Phase::WritingManifest => "Writing Manifest",
        // Restore-only phases; the `Progress` trait is shared with restore but a backup run
        // never reports these.
        Phase::VerifyingArchive | Phase::Restoring => "Working",
    }
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based). Mirrors `select_cmd`'s and
/// `where_screen`'s own size rendering so every screen reads sizes the same way; kept local
/// rather than shared to avoid coupling the screen modules.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

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
    use linux_backup_core::location::FreeSpaceWarning;
    use linux_backup_core::manifest::{Manifest, PackageInfo, VanishedEntry, SCHEMA_VERSION};
    use linux_backup_core::model::{Entry, EntryRoot};
    use secrecy::SecretString;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/does-not-need-to-exist/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            category,
            sensitive,
        }
    }

    fn location(backup_dir: &str) -> LocationSelection {
        LocationSelection {
            location: PathBuf::from("/run/media/youri/USB"),
            backup_dir: PathBuf::from(backup_dir),
            warning: None,
        }
    }

    fn manifest_with(
        vanished: Vec<VanishedEntry>,
        packages: BTreeMap<String, Vec<PackageInfo>>,
    ) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
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
            vanished,
            packages,
        }
    }

    // --- category_breakdown ----------------------------------------------------------

    #[test]
    fn category_breakdown_counts_entries_per_category_sorted_by_name() {
        let selection = SelectionModel::new(vec![
            entry("SSH", true, ".ssh/id_ed25519"),
            entry("SSH", true, ".ssh/config"),
            entry("Shell", false, "aliases.zsh"),
        ]);

        assert_eq!(
            category_breakdown(&selection),
            vec![("SSH", 2), ("Shell", 1)]
        );
    }

    // --- review_text -----------------------------------------------------------------

    #[test]
    fn review_text_summarizes_selection_location_and_encryption() {
        let selection = SelectionModel::new(vec![
            entry("SSH", true, ".ssh/id_ed25519"),
            entry("Shell", false, "aliases.zsh"),
        ]);
        let location = location("/run/media/youri/USB/linux-backup-host-42");
        let encryption = Encryption::Passphrase(SecretString::from("hunter2hunter2!"));

        let review = review_text(&selection, &location, &encryption, false);

        assert!(review.contains("2 Entries across 2 Categories"));
        assert!(review.contains("SSH: 1 Entry"));
        assert!(review.contains("Shell: 1 Entry"));
        assert!(review.contains("Output location: /run/media/youri/USB"));
        assert!(review.contains("Backup dir:      /run/media/youri/USB/linux-backup-host-42"));
        assert!(review.contains("encrypted with a passphrase"));
    }

    #[test]
    fn review_text_surfaces_the_free_space_warning_when_present() {
        let selection = SelectionModel::new(vec![entry("Shell", false, "aliases.zsh")]);
        let mut location = location("/run/media/youri/USB/linux-backup-host-42");
        location.warning = Some(FreeSpaceWarning {
            selection_size: 950,
            available_space: 1000,
        });

        let review = review_text(&selection, &location, &Encryption::None, false);

        assert!(review.contains("close to this location's free space"));
    }

    // --- encryption_review_line ------------------------------------------------------

    #[test]
    fn encryption_review_line_names_passphrase_encryption() {
        let selection = SelectionModel::new(vec![entry("SSH", true, ".ssh/id_ed25519")]);
        let line = encryption_review_line(
            &selection,
            &Encryption::Passphrase(SecretString::from("pw")),
            false,
        );
        assert!(line.contains("encrypted with a passphrase"));
    }

    #[test]
    fn encryption_review_line_names_exposed_sensitive_categories_when_auto_insist_disabled() {
        // A Sensitive Selection written in plaintext: Auto-insist was deliberately disabled, so
        // REVIEW must name the exposed Sensitive Categories, not just say "plaintext".
        let selection = SelectionModel::new(vec![
            entry("SSH", true, ".ssh/id_ed25519"),
            entry("GPG", true, ".gnupg/pubring.kbx"),
        ]);

        let line = encryption_review_line(&selection, &Encryption::None, true);

        assert!(line.contains("PLAINTEXT"));
        assert!(line.contains("Auto-insist disabled"));
        assert!(line.contains("SSH"));
        assert!(line.contains("GPG"));
    }

    #[test]
    fn encryption_review_line_is_plain_when_nothing_is_sensitive() {
        let selection = SelectionModel::new(vec![entry("Shell", false, "aliases.zsh")]);
        let line = encryption_review_line(&selection, &Encryption::None, false);
        assert!(line.contains("plaintext"));
        assert!(!line.contains("PLAINTEXT"));
    }

    // --- precheck_collision ----------------------------------------------------------

    #[test]
    fn precheck_collision_is_clear_when_the_backup_dir_does_not_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fresh = tmp.path().join("linux-backup-host-1");
        assert_eq!(precheck_collision(&fresh), CollisionCheck::Clear);
    }

    #[test]
    fn precheck_collision_stops_with_a_message_when_the_backup_dir_already_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let existing = tmp.path().join("linux-backup-host-1");
        std::fs::create_dir(&existing).expect("create colliding dir");

        match precheck_collision(&existing) {
            CollisionCheck::Collision(message) => {
                assert!(message.contains("already exists"));
                assert!(message.contains("Nothing was written"));
            }
            CollisionCheck::Clear => panic!("an existing Backup dir must be a collision"),
        }
    }

    // --- phase_label -----------------------------------------------------------------

    #[test]
    fn phase_label_maps_each_backup_phase_to_a_distinct_label() {
        assert_eq!(phase_label(Phase::Scanning), "Reading Entries");
        assert_eq!(phase_label(Phase::WritingArchive), "Writing Archive");
        assert_eq!(phase_label(Phase::WritingManifest), "Writing Manifest");
    }

    // --- success_summary -------------------------------------------------------------

    #[test]
    fn success_summary_reports_dir_encryption_and_no_vanished_or_packages() {
        let outcome = BackupOutcome {
            backup_dir: PathBuf::from("/run/media/youri/USB/linux-backup-host-1"),
            manifest: manifest_with(Vec::new(), BTreeMap::new()),
        };

        let summary = success_summary(&outcome, &Encryption::None);

        assert!(summary.contains("Backup complete: /run/media/youri/USB/linux-backup-host-1"));
        assert!(summary.contains("Encryption: plaintext"));
        assert!(summary.contains("Vanished Entries: none."));
        assert!(summary.contains("Package lists: none captured."));
    }

    #[test]
    fn success_summary_calls_out_sensitive_vanished_entries_by_name() {
        let outcome = BackupOutcome {
            backup_dir: PathBuf::from("/run/media/youri/USB/linux-backup-host-1"),
            manifest: manifest_with(
                vec![
                    VanishedEntry {
                        path: "home/.ssh/id_ed25519".to_string(),
                        sensitive: true,
                    },
                    VanishedEntry {
                        path: "home/.cache/tempfile".to_string(),
                        sensitive: false,
                    },
                ],
                BTreeMap::new(),
            ),
        };

        let summary = success_summary(&outcome, &Encryption::Passphrase(SecretString::from("pw")));

        assert!(summary.contains("Encryption: encrypted"));
        assert!(summary.contains("Vanished Entries (skipped as benign): 2"));
        assert!(summary.contains("home/.ssh/id_ed25519"));
        // The non-sensitive vanished entry must not be named in the sensitive call-out line.
        assert!(!summary.contains(
            "Sensitive Vanished Entries — check these disappeared expectedly: home/.cache/tempfile"
        ));
    }
}
