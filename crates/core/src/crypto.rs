//! Whole-archive passphrase encryption via `age::Encryptor::with_user_passphrase` and
//! `secrecy::SecretString`, plus the Auto-insist policy (does this Selection contain
//! anything Sensitive -> is encryption required).
//!
//! Auto-insist policy is pure logic over a `SelectionModel` and is on-seam / unit-tested;
//! the interactive passphrase prompt itself is off-seam (lives in `crates/tui`).
//!
//! #22 scope: [`evaluate`] computes Auto-insist from a Selection's already-tagged Entries
//! (Category-level OR Safety-net sensitivity, both resolved by `scan` — see that module's
//! `is_safety_net_match`). This module adds no new sensitivity source of its own; it only
//! decides what a Selection's existing tags *imply* for encryption. Real `age` encryption
//! (`Encryption::Passphrase`'s stubbed body in `archive.rs`) is #23's job, not this ticket's.

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoInsistOverride {
    pub disclosed_categories: Vec<&'static str>,
    pub disclosed_entries: Vec<PathBuf>,
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
        assert_eq!(override_.disclosed_categories, vec!["SSH"]);
        assert_eq!(
            override_.disclosed_entries,
            vec![PathBuf::from("id_ed25519")]
        );
    }
}
