//! The gui-side encryption decision (issue #101): mirrors `crates/tui`'s `WizardState`
//! encryption bookkeeping (ADR-0007 D3/D5). `core::crypto::evaluate` is pure Auto-insist
//! policy over a `SelectionModel` and carries no memory of *which* Sensitive set a past
//! plaintext decision covered — that "recorded context" is deliberately a `tui`-side field
//! per ADR-0007's Consequences, and it is equally deliberately a `gui`-side field here: per
//! ADR-0008, `gui` and `tui` share no code directly, so this is an independent
//! re-implementation of the same rule (both derived from `crypto::evaluate`), not an import,
//! and not a `core` change.

use std::collections::BTreeSet;
use std::path::PathBuf;

use linux_backup_core::archive::Encryption;
use linux_backup_core::{crypto, SelectionModel};
use secrecy::SecretString;

/// The Sensitive Entry set of a Selection — ADR-0007 D5's "recorded context" derivation,
/// recomputed from the live Selection rather than cached, so it can never go stale.
fn sensitive_set(selection: &SelectionModel) -> BTreeSet<PathBuf> {
    crypto::evaluate(selection)
        .sensitive_entries
        .into_iter()
        .collect()
}

/// The gui-side encryption decision: the chosen [`Encryption`] plus, while it's plaintext, the
/// Sensitive set that decision was made against. `None` means no plaintext decision has been
/// recorded yet (a Sensitive Selection still awaiting a passphrase or a disable-confirmation)
/// — distinct from `Some(empty set)`, the non-Sensitive default's baseline.
#[derive(Debug, Clone)]
pub struct EncryptionDecision {
    encryption: Encryption,
    plaintext_baseline: Option<BTreeSet<PathBuf>>,
}

impl EncryptionDecision {
    /// Starts plaintext. The baseline (covering the empty set) is pre-recorded only when
    /// nothing is Sensitive yet, so a non-Sensitive Selection opens encryption-complete
    /// (ADR-0007 D4); a Sensitive one starts undecided and must set a passphrase or disable
    /// Auto-insist before the encryption step completes.
    pub fn new(selection: &SelectionModel) -> Self {
        let baseline = sensitive_set(selection).is_empty().then(BTreeSet::new);
        Self {
            encryption: Encryption::None,
            plaintext_baseline: baseline,
        }
    }

    /// Whether a passphrase is currently set (the safe harbor is in effect).
    pub fn has_passphrase(&self) -> bool {
        self.encryption.is_encrypted()
    }

    /// Commits a passphrase — the encryption safe harbor (ADR-0007 D5). Once set, the whole
    /// Archive is encrypted, so no later WHAT edit can re-open the step or discard it.
    pub fn set_passphrase(&mut self, passphrase: SecretString) {
        self.encryption = Encryption::Passphrase(passphrase);
    }

    /// Records a deliberate plaintext decision covering the *current* live Sensitive set — the
    /// non-Sensitive "keep plaintext" path and the Sensitive disable-Auto-insist override both
    /// land here. A later WHAT edit that grows the Sensitive set beyond this baseline
    /// invalidates the decision again (ADR-0007 D5).
    pub fn choose_plaintext(&mut self, selection: &SelectionModel) {
        self.encryption = Encryption::None;
        self.plaintext_baseline = Some(sensitive_set(selection));
    }

    /// ADR-0007 D2/D4/D5's encryption gate: a decision exists. A set passphrase is always
    /// complete (safe harbor); a plaintext decision is complete only while the live Sensitive
    /// set stays within the set it was made against.
    pub fn is_complete(&self, selection: &SelectionModel) -> bool {
        if self.encryption.is_encrypted() {
            return true;
        }
        match &self.plaintext_baseline {
            Some(baseline) => sensitive_set(selection).is_subset(baseline),
            None => false,
        }
    }

    /// ADR-0007 D5: whether a prior *completed* plaintext decision has been overtaken by newly
    /// added Sensitive content it didn't cover. A set passphrase is the safe harbor and is
    /// never invalidated; a Sensitive Selection that was simply never decided (no baseline
    /// yet) is incomplete but not invalidated — there is nothing prior to overtake.
    pub fn is_invalidated(&self, selection: &SelectionModel) -> bool {
        if self.encryption.is_encrypted() {
            return false;
        }
        match &self.plaintext_baseline {
            Some(baseline) => !sensitive_set(selection).is_subset(baseline),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use linux_backup_core::model::{Entry, EntryRoot, EntryType};

    use super::*;

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/home/tester/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive,
        }
    }

    fn non_sensitive_selection() -> SelectionModel {
        SelectionModel::new(vec![entry("Shell", false, ".zshrc")])
    }

    fn sensitive_selection() -> SelectionModel {
        SelectionModel::new(vec![entry("SSH", true, "id_ed25519")])
    }

    #[test]
    fn non_sensitive_selection_is_complete_on_arrival() {
        let selection = non_sensitive_selection();
        let decision = EncryptionDecision::new(&selection);
        assert!(decision.is_complete(&selection));
        assert!(!decision.is_invalidated(&selection));
        assert!(!decision.has_passphrase());
    }

    #[test]
    fn sensitive_selection_starts_incomplete_but_not_invalidated() {
        let selection = sensitive_selection();
        let decision = EncryptionDecision::new(&selection);
        assert!(!decision.is_complete(&selection));
        assert!(!decision.is_invalidated(&selection));
    }

    #[test]
    fn setting_a_passphrase_completes_a_sensitive_selection() {
        let selection = sensitive_selection();
        let mut decision = EncryptionDecision::new(&selection);
        decision.set_passphrase(SecretString::from("correct-horse-battery-staple9!"));
        assert!(decision.is_complete(&selection));
        assert!(decision.has_passphrase());
    }

    #[test]
    fn adding_sensitive_content_invalidates_a_plaintext_decision() {
        let mut selection = non_sensitive_selection();
        let decision = EncryptionDecision::new(&selection);
        assert!(decision.is_complete(&selection));

        selection.add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );

        assert!(!decision.is_complete(&selection));
        assert!(decision.is_invalidated(&selection));
    }

    #[test]
    fn a_set_passphrase_is_a_safe_harbor_against_invalidation() {
        let mut selection = non_sensitive_selection();
        let mut decision = EncryptionDecision::new(&selection);
        decision.set_passphrase(SecretString::from("correct-horse-battery-staple9!"));

        selection.add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );

        assert!(decision.is_complete(&selection));
        assert!(!decision.is_invalidated(&selection));
    }

    #[test]
    fn disabling_auto_insist_covers_the_named_set_but_not_a_later_addition() {
        let mut selection = sensitive_selection();
        let mut decision = EncryptionDecision::new(&selection);
        assert!(!decision.is_complete(&selection));

        decision.choose_plaintext(&selection);
        assert!(decision.is_complete(&selection));
        assert!(!decision.is_invalidated(&selection));

        selection.add_custom_path(
            Path::new("/home/tester/.aws/credentials"),
            Path::new("/home/tester"),
        );
        assert!(decision.is_invalidated(&selection));
    }
}
