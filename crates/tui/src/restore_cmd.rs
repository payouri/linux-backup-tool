//! The `linux-backup restore <dir> [--verify] [--overwrite]` command (#29) — the first real
//! thing this binary does. This module orchestrates calls into `core::restore`'s public API
//! only; per ADR-0001 ("tui is a thin view"), no restore logic is reimplemented here.
//!
//! Story 50 is the load-bearing ordering constraint: when the Backup is encrypted, the
//! passphrase prompt must complete *before* `core::restore::restore` is called at all, so
//! the user learns whether they have the right passphrase before a single file is touched.
//! [`run`] enforces this structurally — `is_encrypted_backup` and `prompt_passphrase` both
//! run to completion ahead of the one call into `restore::restore`, which is the only place
//! in this module (or `core`) that writes anything to disk.
//!
//! Story 54 / issue #37's "confirm once" collision handling lives here: when a restore would
//! overwrite existing file(s) and `--overwrite` was not passed, this module shows a single
//! `inquire::Confirm` naming every colliding target before anything is clobbered. It plugs
//! into `core::restore`'s [`CollisionPolicy::Confirm`](restore::CollisionPolicy) callback —
//! `core` pre-scans the collisions and hands them here as data; the `inquire` prompt itself
//! never crosses into `core` (ADR-0001). `--overwrite` bypasses the prompt entirely
//! ([`CollisionPolicy::Overwrite`](restore::CollisionPolicy)), preserving the unchanged
//! non-interactive AFK behaviour.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use inquire::{Confirm, Password};
use linux_backup_core::restore::{
    self, CollisionPolicy, OwnershipOutcome, RestoreOutcome, VerifyReport,
};
use linux_backup_core::{archive, CoreError, Privilege};
use secrecy::SecretString;

use crate::cli::RestoreArgs;
use crate::system;

pub fn run(args: &RestoreArgs) -> Result<()> {
    if !args.backup_dir.is_dir() {
        bail!(
            "backup dir {} does not exist or is not a directory",
            args.backup_dir.display()
        );
    }

    let passphrase = if is_encrypted_backup(&args.backup_dir) {
        Some(prompt_passphrase()?)
    } else {
        None
    };

    let ctx = system::build_run_context()?;

    // Story 53: the user must learn the ownership outcome *before* Restore proceeds, not
    // just read it back afterward in the summary — `restore.rs`'s own doc comment (#26)
    // calls this "up front" warning out as this command's (#29's) job to deliver.
    if let Some(warning) = ownership_up_front_warning(ctx.privilege) {
        println!("{warning}");
    }

    // `--overwrite` overwrites non-interactively (unchanged AFK behaviour); without it, a
    // collision triggers the single "confirm once" prompt below (issue #37). The prompt fires
    // only if `core`'s pre-scan actually finds a collision, and after the passphrase step
    // above — so nothing is written before the user has confirmed.
    let mut confirm = confirm_overwrite;
    let policy = if args.overwrite {
        CollisionPolicy::Overwrite
    } else {
        CollisionPolicy::Confirm(&mut confirm)
    };

    let outcome = restore::restore(&args.backup_dir, &ctx, passphrase.as_ref(), policy)
        .map_err(describe_restore_error)?;

    println!("{}", format_summary(&outcome));

    if args.verify {
        let report = restore::verify(&ctx, &outcome.manifest);
        println!("{}", format_verify_report(&report));
    }

    Ok(())
}

/// Whether `backup_dir` is encrypted, detected the same way `core::restore` itself does
/// internally: the `.age`-suffixed Manifest filename is the authoritative signal
/// (CONTEXT.md's Manifest entry — never a field inside the Manifest). Checking it here,
/// ahead of calling `restore::restore` at all, is what makes story 50's ordering possible:
/// `restore::restore` only discovers encryption *after* it's already been called, which
/// would be too late for an up-front prompt.
fn is_encrypted_backup(backup_dir: &Path) -> bool {
    backup_dir
        .join(archive::filename_for(archive::MANIFEST_FILENAME, true))
        .is_file()
}

/// Story 53's up-front ownership warning: `None` when running as root (nothing to warn
/// about — ownership *will* be restored), `Some(message)` otherwise. A pure function of
/// [`Privilege`] alone, so it's unit-testable without touching disk, matching this module's
/// other renderers ([`format_summary`], [`format_verify_report`]).
fn ownership_up_front_warning(privilege: Privilege) -> Option<String> {
    match privilege {
        Privilege::Root => None,
        Privilege::NonRoot => Some(
            "Note: not running as root — restored files will land under your current user; \
             original file ownership (uid/gid) will not be restored."
                .to_string(),
        ),
    }
}

/// Prompts for the passphrase via `inquire::Password`, holding it in a `secrecy::SecretString`
/// end-to-end (never a plain `String`) per the parent spec's crypto crate-choice bullet.
/// No confirmation prompt: unlike backup-time passphrase entry (story 20, guarding against a
/// typo locking in a wrong passphrase), a wrong restore passphrase fails cleanly against the
/// real ciphertext with nothing lost — a second prompt would only add friction.
fn prompt_passphrase() -> Result<SecretString> {
    let raw = Password::new("This backup is encrypted. Enter its passphrase:")
        .without_confirmation()
        .prompt()
        .context("failed to read passphrase")?;
    Ok(SecretString::from(raw))
}

/// The single "confirm once" collision prompt (issue #37): `core` calls this at most once,
/// with every colliding target, when a non-`--overwrite` restore would overwrite existing
/// file(s). Returns `true` to overwrite them all, `false` to skip them all (leaving the
/// existing files untouched and reported as skipped). A prompt that can't be answered — no
/// TTY, or the user aborts with Esc/Ctrl-C — defaults to `false`: the safe choice never
/// clobbers anything the user didn't actively confirm.
fn confirm_overwrite(collisions: &[PathBuf]) -> bool {
    println!("{}", format_collision_list(collisions));
    Confirm::new("Overwrite the existing file(s) listed above?")
        .with_default(false)
        .prompt()
        .unwrap_or(false)
}

/// Renders the collision list the confirmation names — the "what would be overwritten" the
/// acceptance criteria require shown before writing. A pure function of the colliding paths
/// so it's unit-testable without a TTY, matching this module's other renderers.
fn format_collision_list(collisions: &[PathBuf]) -> String {
    let noun = if collisions.len() == 1 {
        "file"
    } else {
        "files"
    };
    let mut lines = vec![format!(
        "{} existing {noun} would be overwritten by this restore:",
        collisions.len()
    )];
    for path in collisions {
        lines.push(format!("  - {}", path.display()));
    }
    lines.join("\n")
}

/// Wraps a restore failure with a hint for the one case a user is most likely to hit and be
/// confused by: a wrong passphrase surfaces as `CoreError::DecryptionFailed` with no further
/// detail (deliberately — `age` doesn't distinguish "wrong passphrase" from "corrupted
/// ciphertext" to avoid leaking oracle information), so this is where that ambiguity gets
/// explained to a human instead of left as a bare error string.
fn describe_restore_error(err: CoreError) -> anyhow::Error {
    let needs_hint = matches!(err, CoreError::DecryptionFailed(_));
    let base = anyhow::Error::from(err);
    if needs_hint {
        base.context(
            "hint: this usually means the passphrase was wrong, or the backup dir's archive/\
             manifest is corrupted. No files were touched.",
        )
    } else {
        base
    }
}

/// Renders the post-restore summary (per-file failures, ownership outcome, `reinstall.sh`
/// per-manager counts) that the acceptance criteria require printed at the end of a run.
/// A pure function of [`RestoreOutcome`] so it's unit-testable without touching disk.
fn format_summary(outcome: &RestoreOutcome) -> String {
    let mut lines = vec![format!(
        "Restored {} file(s).",
        outcome.restored_paths.len()
    )];

    lines.push(match outcome.ownership {
        OwnershipOutcome::Restored => {
            "Ownership: restored to each file's original uid/gid (running as root).".to_string()
        }
        OwnershipOutcome::SkippedNonRoot => "Ownership: left under the current user (not \
             running as root); original uid/gid was not restored."
            .to_string(),
    });

    // Issue #37: a declined collision is a deliberate skip, reported apart from real errors so
    // the user can tell "I chose not to overwrite this" from "this failed".
    if outcome.skipped_collisions.is_empty() {
        lines.push("Skipped (collision, overwrite declined): none.".to_string());
    } else {
        lines.push(format!(
            "Skipped ({}, existing files left untouched — overwrite declined):",
            outcome.skipped_collisions.len()
        ));
        for skipped in &outcome.skipped_collisions {
            lines.push(format!("  - {}", skipped.path));
        }
    }

    if outcome.failures.is_empty() {
        lines.push("Failures: none.".to_string());
    } else {
        lines.push(format!("Failures ({}):", outcome.failures.len()));
        for failure in &outcome.failures {
            lines.push(format!("  - {}: {}", failure.path, failure.reason));
        }
    }

    lines.push(format!(
        "reinstall.sh written to {} — review it yourself; linux-backup never runs it.",
        outcome.reinstall_script_path.display()
    ));
    if outcome.package_counts.is_empty() {
        lines.push("  (no package managers were captured in this Backup's Manifest)".to_string());
    } else {
        for (manager, count) in &outcome.package_counts {
            lines.push(format!("  - {manager}: {count} package(s)"));
        }
    }

    lines.join("\n")
}

/// Renders the opt-in `--verify` pass's report (story 57). A pure function of
/// [`VerifyReport`] so it's unit-testable without touching disk.
fn format_verify_report(report: &VerifyReport) -> String {
    let mut lines = vec![format!(
        "Verify: {} matched, {} mismatched, {} missing.",
        report.matched.len(),
        report.mismatched.len(),
        report.missing.len()
    )];

    if report.is_clean() {
        lines.push("Verify: every checksummed file matched the Manifest.".to_string());
        return lines.join("\n");
    }

    for mismatch in &report.mismatched {
        lines.push(format!(
            "  - MISMATCH {}: expected {}, got {}",
            mismatch.path, mismatch.expected_sha256, mismatch.actual_sha256
        ));
    }
    for missing in &report.missing {
        lines.push(format!("  - MISSING {missing}"));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use linux_backup_core::manifest::Manifest;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn empty_manifest() -> Manifest {
        Manifest {
            schema_version: linux_backup_core::manifest::SCHEMA_VERSION,
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

    #[test]
    fn ownership_up_front_warning_is_none_when_running_as_root() {
        assert_eq!(ownership_up_front_warning(Privilege::Root), None);
    }

    #[test]
    fn ownership_up_front_warning_names_the_ownership_outcome_when_not_root() {
        let warning = ownership_up_front_warning(Privilege::NonRoot).expect("non-root must warn");
        assert!(warning.contains("not running as root"));
        assert!(warning.contains("uid/gid"));
    }

    #[test]
    fn is_encrypted_backup_is_true_when_the_age_suffixed_manifest_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("manifest.json.age"), b"ignored").expect("write");
        assert!(is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn is_encrypted_backup_is_false_when_only_the_plain_manifest_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("manifest.json"), b"ignored").expect("write");
        assert!(!is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn is_encrypted_backup_is_false_when_the_dir_has_neither_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn format_summary_reports_no_failures_and_ownership_restored() {
        let outcome = RestoreOutcome {
            manifest: empty_manifest(),
            restored_paths: vec![PathBuf::from("/home/user/.bashrc")],
            failures: Vec::new(),
            skipped_collisions: Vec::new(),
            ownership: OwnershipOutcome::Restored,
            reinstall_script_path: PathBuf::from("/home/user/reinstall.sh"),
            package_counts: BTreeMap::from([("pacman".to_string(), 3)]),
        };

        let summary = format_summary(&outcome);

        assert!(summary.contains("Restored 1 file(s)."));
        assert!(summary.contains("Failures: none."));
        assert!(summary.contains("restored to each file's original uid/gid"));
        assert!(summary.contains("pacman: 3 package(s)"));
        assert!(summary.contains("reinstall.sh written to /home/user/reinstall.sh"));
    }

    #[test]
    fn format_summary_lists_every_per_file_failure_and_notes_non_root_ownership() {
        let outcome = RestoreOutcome {
            manifest: empty_manifest(),
            restored_paths: Vec::new(),
            failures: vec![restore::RestoreFailure {
                path: "/home/user/.ssh/id_ed25519".to_string(),
                reason: "ownership restore failed: EPERM".to_string(),
            }],
            skipped_collisions: vec![restore::SkippedCollision {
                path: "/home/user/.bashrc".to_string(),
            }],
            ownership: OwnershipOutcome::SkippedNonRoot,
            reinstall_script_path: PathBuf::from("/home/user/reinstall.sh"),
            package_counts: BTreeMap::new(),
        };

        let summary = format_summary(&outcome);

        // A declined collision and a real failure are reported in separate sections (issue
        // #37), never conflated into one list.
        assert!(summary.contains("Skipped (1"));
        assert!(summary.contains("overwrite declined"));
        assert!(summary.contains("  - /home/user/.bashrc"));
        assert!(summary.contains("Failures (1):"));
        assert!(summary.contains("/home/user/.ssh/id_ed25519: ownership restore failed: EPERM"));
        assert!(summary.contains("Ownership: left under the current user"));
        assert!(summary.contains("original uid/gid was not restored"));
        assert!(summary.contains("no package managers were captured"));
    }

    #[test]
    fn format_collision_list_names_every_would_be_overwritten_path() {
        let rendered = format_collision_list(&[
            PathBuf::from("/home/user/.bashrc"),
            PathBuf::from("/home/user/.ssh/id_ed25519"),
        ]);
        assert!(rendered.contains("2 existing files would be overwritten"));
        assert!(rendered.contains("  - /home/user/.bashrc"));
        assert!(rendered.contains("  - /home/user/.ssh/id_ed25519"));
    }

    #[test]
    fn format_collision_list_uses_singular_noun_for_one_collision() {
        let rendered = format_collision_list(&[PathBuf::from("/home/user/.bashrc")]);
        assert!(rendered.contains("1 existing file would be overwritten"));
    }

    #[test]
    fn format_verify_report_is_clean_when_everything_matched() {
        let report = VerifyReport {
            matched: vec!["home/.bashrc".to_string()],
            mismatched: Vec::new(),
            missing: Vec::new(),
        };

        let rendered = format_verify_report(&report);

        assert!(rendered.contains("1 matched, 0 mismatched, 0 missing"));
        assert!(rendered.contains("every checksummed file matched"));
    }

    #[test]
    fn format_verify_report_lists_mismatches_and_missing_paths() {
        let report = VerifyReport {
            matched: Vec::new(),
            mismatched: vec![restore::VerifyMismatch {
                path: "home/tampered.txt".to_string(),
                expected_sha256: "aaa".to_string(),
                actual_sha256: "bbb".to_string(),
            }],
            missing: vec!["home/gone.txt".to_string()],
        };

        let rendered = format_verify_report(&report);

        assert!(rendered.contains("0 matched, 1 mismatched, 1 missing"));
        assert!(rendered.contains("MISMATCH home/tampered.txt: expected aaa, got bbb"));
        assert!(rendered.contains("MISSING home/gone.txt"));
    }
}
