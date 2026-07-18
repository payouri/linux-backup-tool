//! Passphrase-related policy: the Auto-insist policy (does this Selection contain anything
//! Sensitive -> is encryption required) and the passphrase strength checklist. The actual
//! `age::Encryptor::with_user_passphrase` encryption pipeline lives in `archive.rs`
//! (`Encryption::Passphrase`'s `encrypt`/`decrypt`) — this module only decides policy
//! *around* a passphrase, never performs the encryption itself.
//!
//! Auto-insist policy is pure logic over a `SelectionModel` and is on-seam / unit-tested;
//! the interactive passphrase prompt itself is off-seam (lives in `crates/tui`).
//!
//! #22 scope: [`evaluate`] computes Auto-insist from a Selection's already-tagged Entries
//! (Category-level, per-entry tag, or Safety-net sensitivity — all three resolved upstream by
//! `catalog`/`scan`, see `catalog::entry_is_individually_sensitive` and
//! `scan::is_safety_net_match`). This module adds no new sensitivity source of its own; it
//! only decides what a Selection's existing tags *imply* for encryption.
//!
//! #23 scope: [`passphrase_strength`] is the pure-function half of story 20's live,
//! non-blocking strength checklist — per-criterion, not a single boolean, since the TUI
//! (#31) renders each of the three criteria independently rather than gating on them.
//! ("Live" is per-entry, not per-keystroke: `inquire` 0.9's `Password` exposes no
//! per-keystroke hook, so #31 recomputes and reshows the checklist after each entry.)
//! The hard, non-advisory blank-passphrase reject (story 21) is deliberately *not*
//! here: it lives in `archive::encrypt`, the point where a passphrase is actually used,
//! not this advisory checklist.

use std::path::PathBuf;

use crate::model::SelectionModel;

/// The result of evaluating Auto-insist over a Selection (CONTEXT.md's "Auto-insist"):
/// whether whole-archive encryption is required, plus exactly which Sensitive Categories
/// and Entries triggered it — so a TUI (#31) can show that list before prompting for a
/// passphrase (story 18) without re-deriving it from the Selection itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoInsist {
    /// Whether the Selection contains anything Sensitive (story 15/17): any Entry from a
    /// sensitive Category, a per-entry sensitivity tag, or a Safety-net match.
    pub encryption_required: bool,
    /// Sensitive Category names present in the Selection, deduplicated, in Selection order.
    pub sensitive_categories: Vec<&'static str>,
    /// Sensitive Entries' relative (Manifest-style) paths, for naming individual files if a
    /// TUI wants finer granularity than just Category names.
    pub sensitive_entries: Vec<PathBuf>,
}

impl AutoInsist {
    /// Disables Auto-insist, producing the disclosure a second-confirmation UI (#31) must
    /// show before honoring the override — the exact Sensitive content that would land in
    /// plaintext (story 19). Returns `None` when nothing is Sensitive in the first place:
    /// there is nothing to disclose or override, so the escape hatch has no effect.
    ///
    /// Deliberately returns the disclosure paired with the override rather than a bare
    /// `bool`, so a caller can never opt out of Auto-insist without also holding the list of
    /// what it's opting out of protecting — the TUI's confirmation dialog reads this
    /// struct's fields directly instead of re-computing them from the Selection.
    pub fn disable(&self) -> Option<AutoInsistOverride> {
        if !self.encryption_required {
            return None;
        }
        Some(AutoInsistOverride {
            disclosed_categories: self.sensitive_categories.clone(),
            disclosed_entries: self.sensitive_entries.clone(),
        })
    }
}

/// The result of a user disabling Auto-insist (story 19): the disclosure a
/// second-confirmation TUI must name before the plaintext archive is actually written.
///
/// Fields are deliberately private with read-only accessors, not `pub`: the doc comment on
/// [`AutoInsist::disable`] promises a caller can never opt out of Auto-insist without also
/// holding the disclosure of what it's opting out of protecting. If the fields were `pub`,
/// that would only be a convention — any caller could construct an `AutoInsistOverride` with
/// empty (or fabricated) disclosure lists directly, bypassing `disable()` entirely. Making
/// the fields private and the struct constructible only from this module means `disable()`
/// is the *sole* way to obtain one, so the guarantee is enforced by the type system rather
/// than by nobody happening to reach for a struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoInsistOverride {
    disclosed_categories: Vec<&'static str>,
    disclosed_entries: Vec<PathBuf>,
}

impl AutoInsistOverride {
    /// The Sensitive Category names that would land in plaintext if this override is honored.
    pub fn disclosed_categories(&self) -> &[&'static str] {
        &self.disclosed_categories
    }

    /// The Sensitive Entries' relative paths that would land in plaintext if this override is
    /// honored.
    pub fn disclosed_entries(&self) -> &[PathBuf] {
        &self.disclosed_entries
    }
}

/// Computes Auto-insist policy for `selection`: pure logic over its Entries' already-tagged
/// `sensitive` bool (set by `scan::scan_category`/`scan_taxonomy` from Category-level and
/// Safety-net sources) — no I/O, no re-scanning, no new sensitivity classification of its
/// own.
pub fn evaluate(selection: &SelectionModel) -> AutoInsist {
    let mut categories: Vec<&'static str> = Vec::new();
    let mut entries: Vec<PathBuf> = Vec::new();

    for entry in selection.entries() {
        if !entry.sensitive {
            continue;
        }
        entries.push(entry.relative_path.clone());
        if !categories.contains(&entry.category) {
            categories.push(entry.category);
        }
    }

    AutoInsist {
        encryption_required: !entries.is_empty(),
        sensitive_categories: categories,
        sensitive_entries: entries,
    }
}

/// Story 20's live, non-blocking passphrase-strength checklist: which of the three
/// advisory criteria `candidate` currently satisfies (>=12 chars, >=1 digit, >=1 special
/// character). Per-criterion rather than a single boolean so a TUI can render each item
/// independently in its checklist. None of these being unmet ever blocks encryption — only a
/// literally blank passphrase does, and that hard reject lives in `archive::encrypt`, not
/// here (see this module's doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassphraseStrength {
    /// At least 12 characters.
    pub min_length: bool,
    /// At least one ASCII digit.
    pub has_digit: bool,
    /// At least one character that's neither an ASCII letter nor an ASCII digit
    /// (punctuation, symbols, or whitespace all count).
    pub has_special: bool,
}

impl PassphraseStrength {
    /// Whether every advisory criterion is met. Never consulted to gate encryption
    /// (per this module's doc); only for a TUI that wants one combined "looks strong"
    /// signal alongside the per-criterion checklist.
    pub fn all_met(self) -> bool {
        self.min_length && self.has_digit && self.has_special
    }
}

/// Computes [`PassphraseStrength`] for `candidate`. Pure and non-blocking: called by a TUI
/// passphrase prompt (#31) to reshow the checklist after each entry, never itself
/// rejecting anything — a blank
/// candidate simply reports all three criteria unmet here, and is separately hard-rejected
/// at `archive::encrypt` time (story 21), not by this function.
pub fn passphrase_strength(candidate: &str) -> PassphraseStrength {
    PassphraseStrength {
        min_length: candidate.chars().count() >= 12,
        has_digit: candidate.chars().any(|c| c.is_ascii_digit()),
        has_special: candidate.chars().any(|c| !c.is_ascii_alphanumeric()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, EntryRoot};
    use std::path::PathBuf;

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/tmp/does-not-need-to-exist/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            category,
            sensitive,
        }
    }

    #[test]
    fn evaluate_leaves_encryption_not_required_with_no_sensitive_entries() {
        let selection = SelectionModel::new(vec![
            entry("Shell", false, "aliases.zsh"),
            entry("Program configs", false, ".bashrc"),
        ]);

        let policy = evaluate(&selection);

        assert!(!policy.encryption_required);
        assert!(policy.sensitive_categories.is_empty());
        assert!(policy.sensitive_entries.is_empty());
    }

    #[test]
    fn evaluate_requires_encryption_for_a_sensitive_category_entry() {
        let selection = SelectionModel::new(vec![
            entry("Shell", false, "aliases.zsh"),
            entry("SSH", true, "id_ed25519"),
        ]);

        let policy = evaluate(&selection);

        assert!(policy.encryption_required);
        assert_eq!(policy.sensitive_categories, vec!["SSH"]);
        assert_eq!(policy.sensitive_entries, vec![PathBuf::from("id_ed25519")]);
    }

    #[test]
    fn evaluate_requires_encryption_for_an_entry_flagged_only_by_safety_net() {
        // The Category itself ("Program configs") isn't Sensitive; only this one Entry's
        // filename tripped the Safety-net scan (already resolved upstream by `scan`). This
        // proves Auto-insist reacts to per-entry sensitivity, not just Category-level tags.
        let selection = SelectionModel::new(vec![entry(
            "Program configs",
            true,
            ".config/git/credentials",
        )]);

        let policy = evaluate(&selection);

        assert!(policy.encryption_required);
        assert_eq!(policy.sensitive_categories, vec!["Program configs"]);
        assert_eq!(
            policy.sensitive_entries,
            vec![PathBuf::from(".config/git/credentials")]
        );
    }

    #[test]
    fn evaluate_deduplicates_categories_across_multiple_sensitive_entries() {
        let selection = SelectionModel::new(vec![
            entry("SSH", true, "id_ed25519"),
            entry("SSH", true, "config"),
            entry("GPG", true, "pubring.kbx"),
        ]);

        let policy = evaluate(&selection);

        assert_eq!(policy.sensitive_categories, vec!["SSH", "GPG"]);
        assert_eq!(policy.sensitive_entries.len(), 3);
    }

    #[test]
    fn disable_returns_none_when_nothing_is_sensitive() {
        let selection = SelectionModel::new(vec![entry("Shell", false, "aliases.zsh")]);
        let policy = evaluate(&selection);

        assert!(policy.disable().is_none());
    }

    #[test]
    fn disable_carries_the_full_disclosure_when_something_is_sensitive() {
        let selection = SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]);
        let policy = evaluate(&selection);

        let override_ = policy.disable().expect("should disclose sensitive content");
        assert_eq!(override_.disclosed_categories(), &["SSH"]);
        assert_eq!(
            override_.disclosed_entries(),
            &[PathBuf::from("id_ed25519")]
        );
    }

    #[test]
    fn auto_insist_override_is_only_constructible_via_disable() {
        // No struct literal for `AutoInsistOverride` is reachable from outside this module
        // (its fields are private) — this test exists so that claim stays checked by the
        // compiler rather than living only in a doc comment. If this test still compiles
        // after someone makes the fields `pub`, that alone doesn't prove the guarantee is
        // broken, but it does mean this test stopped being able to catch it — the real
        // enforcement is `disclosed_categories`/`disclosed_entries` staying private.
        let selection = SelectionModel::new(vec![entry("GPG", true, "pubring.kbx")]);
        let policy = evaluate(&selection);
        let override_ = policy.disable().expect("should disclose sensitive content");

        assert_eq!(override_.disclosed_categories(), &["GPG"]);
    }

    #[test]
    fn passphrase_strength_reports_no_criteria_met_for_a_blank_candidate() {
        let strength = passphrase_strength("");

        assert!(!strength.min_length);
        assert!(!strength.has_digit);
        assert!(!strength.has_special);
        assert!(!strength.all_met());
    }

    #[test]
    fn passphrase_strength_flags_each_criterion_independently() {
        // 11 chars: one short of the >=12 minimum, no digit, no special character.
        assert_eq!(
            passphrase_strength("shortphrase"),
            PassphraseStrength {
                min_length: false,
                has_digit: false,
                has_special: false,
            }
        );

        // Exactly 12 chars, still no digit/special.
        assert_eq!(
            passphrase_strength("twelvecharsx"),
            PassphraseStrength {
                min_length: true,
                has_digit: false,
                has_special: false,
            }
        );

        // Long enough and has a digit, but no special character yet.
        assert_eq!(
            passphrase_strength("longenoughphrase1"),
            PassphraseStrength {
                min_length: true,
                has_digit: true,
                has_special: false,
            }
        );
    }

    #[test]
    fn passphrase_strength_all_met_when_every_criterion_is_satisfied() {
        let strength = passphrase_strength("correct-horse-battery-staple9!");

        assert!(strength.min_length);
        assert!(strength.has_digit);
        assert!(strength.has_special);
        assert!(strength.all_met());
    }

    #[test]
    fn passphrase_strength_is_advisory_only_and_never_appears_in_a_hard_reject() {
        // Documents the split this module's doc promises: a weak-by-every-metric
        // passphrase is still a valid `PassphraseStrength` value (all fields false), never
        // a `Result::Err` — only `archive::encrypt`'s blank check is a hard reject.
        let strength = passphrase_strength("weak");
        assert!(!strength.all_met());
    }
}
