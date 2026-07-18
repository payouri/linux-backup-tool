//! The encryption step of the wizard (#31) — the screen that sits between WHAT (#30) and
//! WHERE (#32), appearing only when a Sensitive Selection triggers Auto-insist. Given the
//! `SelectionModel` that WHAT produces, it decides the run's [`Encryption`]: either a
//! passphrase the user sets here, or — behind a second, plaintext-naming confirmation —
//! `Encryption::None` when they deliberately disable Auto-insist.
//!
//! Per ADR-0001 ("tui is a thin view over core"), every *policy* decision is delegated to
//! `core::crypto`: whether encryption is required ([`crypto::evaluate`]), which Sensitive
//! Categories/Entries triggered it (the returned [`AutoInsist`]), the type-safe disable path
//! ([`AutoInsist::disable`] → [`AutoInsistOverride`]), and the advisory strength checklist
//! ([`crypto::passphrase_strength`]). This module owns only the screen: rendering that policy
//! and driving `inquire` prompts.
//!
//! ## Ordering guarantee (story 18, a hard acceptance criterion)
//!
//! The summary naming the triggering Sensitive Categories must print *before* any passphrase
//! prompt. [`run`] enforces this structurally: it prints [`summary_text`] as an unconditional
//! `println!` at the top of the required-encryption branch, before control ever reaches the
//! choice loop that owns the [`Password`] prompt. There is no code path that reaches a prompt
//! without having printed the summary first.
//!
//! ## Why the strength checklist is shown after entry, not per-keystroke
//!
//! Story 20 asks for a "live" strength checklist. `inquire` 0.9's [`Password`] renders its
//! own fixed input line with no per-keystroke hook for custom rendering (its `formatter`
//! only runs on the already-submitted value, its `validator` only surfaces a message when it
//! *rejects*). So "live" here means the same thing #30's WHAT footer means: recomputed and
//! reshown on each loop iteration — the checklist is printed from [`crypto::passphrase_strength`]
//! right after each entry, and a not-fully-met passphrase can be re-entered. It stays
//! advisory (a weak passphrase can be accepted), while a *blank* passphrase is hard-rejected
//! by the prompt's own validator (and again at `archive::encrypt`, story 21).

use std::fmt;

use anyhow::{Context, Result};
use inquire::validator::Validation;
use inquire::{Confirm, Password, Select};
use linux_backup_core::archive::Encryption;
use linux_backup_core::crypto::{self, AutoInsist, AutoInsistOverride, PassphraseStrength};
use linux_backup_core::model::SelectionModel;
use secrecy::SecretString;

/// The encryption screen's result: the run's [`Encryption`] plus whether the user reached it
/// by deliberately disabling Auto-insist (so a caller/REVIEW screen can surface that choice
/// rather than re-deriving it). `#33` will feed `encryption` straight into `core::backup`.
#[derive(Debug)]
pub struct EncryptionOutcome {
    pub encryption: Encryption,
    /// True only when a Sensitive Selection had Auto-insist and the user overrode it through
    /// the second plaintext-naming confirmation. False both when encryption wasn't required
    /// and when the user set a passphrase.
    pub auto_insist_disabled: bool,
}

/// Runs the encryption screen for `selection`.
///
/// A no-op returning [`Encryption::None`] when the Selection contains nothing Sensitive
/// ([`crypto::evaluate`] reports encryption not required). Otherwise renders the summary
/// (story 18) and drives the passphrase / disable-Auto-insist flows described on this module.
pub fn run(selection: &SelectionModel) -> Result<EncryptionOutcome> {
    let policy = crypto::evaluate(selection);

    if !policy.encryption_required {
        return Ok(EncryptionOutcome {
            encryption: Encryption::None,
            auto_insist_disabled: false,
        });
    }

    // Story 18 / ordering guarantee: the summary prints before any prompt in the loop below.
    println!();
    println!("{}", summary_text(&policy));

    loop {
        match choose_action()? {
            Choice::SetPassphrase => {
                return Ok(EncryptionOutcome {
                    encryption: prompt_passphrase()?,
                    auto_insist_disabled: false,
                });
            }
            Choice::DisableAutoInsist => {
                // Re-loop when the user backs out of the second confirmation, returning them
                // to the choice between setting a passphrase and disabling Auto-insist.
                if let Some(encryption) = confirm_disable_auto_insist(&policy)? {
                    return Ok(EncryptionOutcome {
                        encryption,
                        auto_insist_disabled: true,
                    });
                }
            }
        }
    }
}

/// The two ways forward once Auto-insist has been triggered.
#[derive(Debug, Clone, Copy)]
enum Choice {
    SetPassphrase,
    DisableAutoInsist,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::SetPassphrase => "Set an encryption passphrase (recommended)",
            Self::DisableAutoInsist => {
                "Disable Auto-insist — write the Sensitive content in PLAINTEXT"
            }
        };
        write!(f, "{label}")
    }
}

fn choose_action() -> Result<Choice> {
    Select::new(
        "Encryption is required for this Selection. How do you want to proceed?",
        vec![Choice::SetPassphrase, Choice::DisableAutoInsist],
    )
    .prompt()
    .context("encryption choice prompt failed")
}

/// Entry-then-confirm passphrase prompt (story 20). `inquire`'s [`Password`] does the
/// entry+confirm comparison itself (rejecting a mismatch), and the validator hard-rejects a
/// blank passphrase (story 21). The advisory strength checklist is printed after each entry;
/// a not-fully-met passphrase can be re-entered but is never blocked.
fn prompt_passphrase() -> Result<Encryption> {
    loop {
        let raw = Password::new("Set an encryption passphrase:")
            .with_custom_confirmation_message("Confirm encryption passphrase:")
            .with_custom_confirmation_error_message("The passphrases did not match — try again.")
            .with_validator(|input: &str| {
                if input.is_empty() {
                    Ok(Validation::Invalid("Passphrase must not be blank.".into()))
                } else {
                    Ok(Validation::Valid)
                }
            })
            .with_help_message(
                "A blank passphrase is rejected. The strength checklist below is advisory.",
            )
            .prompt()
            .context("passphrase prompt failed")?;

        let strength = crypto::passphrase_strength(&raw);
        println!("{}", strength_checklist(strength));

        if strength.all_met() || accept_weak_passphrase()? {
            return Ok(Encryption::Passphrase(SecretString::from(raw)));
        }
        // Otherwise re-enter: the loop reissues the entry+confirm prompt.
    }
}

fn accept_weak_passphrase() -> Result<bool> {
    Confirm::new("This passphrase doesn't meet every recommendation. Use it anyway?")
        .with_default(true)
        .prompt()
        .context("weak-passphrase confirmation prompt failed")
}

/// The second, distinct confirmation guarding the Auto-insist override (story 19): names the
/// Sensitive content that would land in plaintext, then requires an explicit yes (defaulting
/// to *no*). Returns `Some(Encryption::None)` only on that explicit confirmation, `None` when
/// the user backs out (so [`run`] returns them to the choice).
fn confirm_disable_auto_insist(policy: &AutoInsist) -> Result<Option<Encryption>> {
    // `disable()` returns `None` only when nothing is Sensitive, which this branch is never
    // reached for (encryption_required was true) — but honor the type rather than asserting.
    let Some(override_) = policy.disable() else {
        return Ok(None);
    };

    println!();
    println!("{}", plaintext_disclosure_text(&override_));

    let confirmed = Confirm::new("Write this Sensitive content to disk in PLAINTEXT?")
        .with_default(false)
        .with_help_message(
            "This deliberately downgrades the backup's protection and cannot be undone.",
        )
        .prompt()
        .context("plaintext-disclosure confirmation prompt failed")?;

    Ok(confirmed.then_some(Encryption::None))
}

/// Story 18's pre-passphrase summary: names the Sensitive Categories (and the count/names of
/// Sensitive Entries) that triggered Auto-insist. Pure so it's unit-tested and so [`run`] can
/// print it as a single statement ahead of any prompt.
fn summary_text(policy: &AutoInsist) -> String {
    let mut out = String::from(
        "Encryption is required (Auto-insist): this Selection contains Sensitive content.\n",
    );
    out.push_str(&format!(
        "Triggering Sensitive Categories: {}\n",
        policy.sensitive_categories.join(", ")
    ));
    out.push_str(&format!(
        "Sensitive Entries ({}): {}",
        policy.sensitive_entries.len(),
        join_entries(&policy.sensitive_entries)
    ));
    out
}

/// Story 20's advisory strength checklist: one line per criterion with a checked/unchecked
/// glyph (mirroring `select_cmd`'s `[x]`/`[ ]` Category glyphs for visual consistency).
fn strength_checklist(strength: PassphraseStrength) -> String {
    let mut out = String::from("Passphrase strength (advisory, non-blocking):\n");
    out.push_str(&format!(
        "{} at least 12 characters\n",
        checkbox(strength.min_length)
    ));
    out.push_str(&format!(
        "{} at least one digit\n",
        checkbox(strength.has_digit)
    ));
    out.push_str(&format!(
        "{} at least one special character",
        checkbox(strength.has_special)
    ));
    out
}

/// Story 19's plaintext-disclosure text: names the Categories and Entries that would be
/// written unencrypted, so the second confirmation is informed rather than a bare y/n.
fn plaintext_disclosure_text(override_: &AutoInsistOverride) -> String {
    let mut out = String::from(
        "WARNING: disabling Auto-insist writes the following Sensitive content in PLAINTEXT:\n",
    );
    out.push_str(&format!(
        "  Categories: {}\n",
        override_.disclosed_categories().join(", ")
    ));
    out.push_str(&format!(
        "  Entries ({}): {}",
        override_.disclosed_entries().len(),
        join_entries(override_.disclosed_entries())
    ));
    out
}

fn checkbox(met: bool) -> &'static str {
    if met {
        "  [x]"
    } else {
        "  [ ]"
    }
}

/// Comma-joins Entry relative paths for display, or a placeholder when there are none (which
/// summary/disclosure text never expects on the required-encryption path, but renders
/// gracefully rather than as an empty tail).
fn join_entries(entries: &[std::path::PathBuf]) -> String {
    if entries.is_empty() {
        return "(none)".to_string();
    }
    entries
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use linux_backup_core::crypto::evaluate;
    use linux_backup_core::model::{Entry, EntryRoot};
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

    #[test]
    fn summary_text_names_every_triggering_category_and_the_entry_count() {
        let selection = SelectionModel::new(vec![
            entry("SSH", true, ".ssh/id_ed25519"),
            entry("GPG", true, ".gnupg/pubring.kbx"),
            entry("Shell", false, "aliases.zsh"),
        ]);
        let policy = evaluate(&selection);

        let summary = summary_text(&policy);

        assert!(summary.contains("Auto-insist"));
        assert!(summary.contains("SSH"));
        assert!(summary.contains("GPG"));
        // The non-Sensitive Category must not be named as a trigger.
        assert!(!summary.contains("Shell"));
        assert!(summary.contains("Sensitive Entries (2)"));
        assert!(summary.contains(".ssh/id_ed25519"));
        assert!(summary.contains(".gnupg/pubring.kbx"));
    }

    #[test]
    fn strength_checklist_marks_each_criterion_independently() {
        let checklist = strength_checklist(PassphraseStrength {
            min_length: true,
            has_digit: false,
            has_special: true,
        });

        assert!(checklist.contains("[x] at least 12 characters"));
        assert!(checklist.contains("[ ] at least one digit"));
        assert!(checklist.contains("[x] at least one special character"));
    }

    #[test]
    fn strength_checklist_marks_all_unmet_for_a_blank_candidate() {
        let checklist = strength_checklist(crypto::passphrase_strength(""));

        assert!(checklist.contains("[ ] at least 12 characters"));
        assert!(checklist.contains("[ ] at least one digit"));
        assert!(checklist.contains("[ ] at least one special character"));
    }

    #[test]
    fn plaintext_disclosure_text_names_the_exposed_categories_and_entries() {
        let selection = SelectionModel::new(vec![
            entry("SSH", true, ".ssh/id_ed25519"),
            entry("GPG", true, ".gnupg/pubring.kbx"),
        ]);
        let policy = evaluate(&selection);
        let override_ = policy
            .disable()
            .expect("a Sensitive Selection must disclose its override");

        let disclosure = plaintext_disclosure_text(&override_);

        assert!(disclosure.contains("PLAINTEXT"));
        assert!(disclosure.contains("SSH"));
        assert!(disclosure.contains("GPG"));
        assert!(disclosure.contains("Entries (2)"));
        assert!(disclosure.contains(".ssh/id_ed25519"));
        assert!(disclosure.contains(".gnupg/pubring.kbx"));
    }

    #[test]
    fn join_entries_renders_a_placeholder_rather_than_an_empty_tail() {
        assert_eq!(join_entries(&[]), "(none)");
    }
}
