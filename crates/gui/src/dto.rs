//! View DTOs crossing the Tauri IPC boundary, built from `core::SelectionModel`'s plain
//! accessors via `From` (ADR-0008 Decision 2). `core` gains zero net-new `serde` derives —
//! serialization lives entirely here, in `gui`.

use linux_backup_core::location::LocationSelection;
use linux_backup_core::{crypto, DirectoryState, SelectionModel};

use crate::encryption::EncryptionDecision;

/// The wire-safe mirror of [`DirectoryState`] (ADR-0006's tri-state, issue #100's per-node
/// state). `lowercase` on the wire (`"all"`/`"partial"`/`"none"`) to read as a plain TS
/// string-literal union on the frontend, matching [`TreeNodeKindDto`]'s own casing choice.
///
/// [`TreeNodeKindDto`]: crate::tree::TreeNodeKindDto
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryStateDto {
    All,
    Partial,
    None,
}

impl From<DirectoryState> for DirectoryStateDto {
    fn from(state: DirectoryState) -> Self {
        match state {
            DirectoryState::All => Self::All,
            DirectoryState::Partial => Self::Partial,
            DirectoryState::None => Self::None,
        }
    }
}

/// The response to the lazy `["selection-summary"]` query (ADR-0008 Decision 2 "Views
/// out"): counts, total size, emptiness, whether Auto-insist requires encryption, and any
/// warnings (CONTEXT.md's "Sensitive"/"Auto-insist"/User-data size warning).
///
/// `camelCase` on the wire: this DTO is consumed by the `TanStack` Query frontend over IPC,
/// not persisted as a file format like the Manifest, so it follows JS naming rather than
/// the Manifest's `snake_case`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionSummaryDto {
    pub entry_count: usize,
    pub total_size_bytes: u64,
    pub is_empty: bool,
    pub encryption_required: bool,
    pub warnings: Vec<String>,
}

impl From<&SelectionModel> for SelectionSummaryDto {
    fn from(selection: &SelectionModel) -> Self {
        Self {
            entry_count: selection.entries().len(),
            total_size_bytes: selection.total_size(),
            is_empty: selection.is_empty(),
            encryption_required: selection.encryption_required(),
            warnings: selection.warnings(),
        }
    }
}

/// The response to the `["encryption-status"]` query (issue #101): the encryption step's
/// derived state, reflecting `core::crypto::evaluate`'s Auto-insist policy plus the gui-side
/// [`EncryptionDecision`]'s completeness/invalidation (ADR-0007 D2/D4/D5). Built from two
/// inputs via a plain function rather than `From`, the same way `tree::category_nodes` takes
/// both a `SelectionModel` and the scanned taxonomy — this DTO cannot be derived from either
/// input alone.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionStatusDto {
    /// Auto-insist (CONTEXT.md): whether the Selection contains anything Sensitive, forcing
    /// whole-archive encryption on.
    pub auto_insist_required: bool,
    /// The Sensitive Category names Auto-insist would name in the override confirmation
    /// (story 25).
    pub sensitive_categories: Vec<String>,
    pub has_passphrase: bool,
    /// The encryption step's forward gate (ADR-0007 D2).
    pub is_complete: bool,
    /// ADR-0007 D5: a completed plaintext decision has been overtaken by newly-added Sensitive
    /// content it didn't cover.
    pub is_invalidated: bool,
}

/// Builds the `["encryption-status"]` response from the live Selection and the gui-side
/// [`EncryptionDecision`] (issue #101).
pub fn encryption_status(
    selection: &SelectionModel,
    decision: &EncryptionDecision,
) -> EncryptionStatusDto {
    let auto_insist = crypto::evaluate(selection);
    EncryptionStatusDto {
        auto_insist_required: auto_insist.encryption_required,
        sensitive_categories: auto_insist
            .sensitive_categories
            .iter()
            .map(ToString::to_string)
            .collect(),
        has_passphrase: decision.has_passphrase(),
        is_complete: decision.is_complete(selection),
        is_invalidated: decision.is_invalidated(selection),
    }
}

/// The response to the `["where-check", location]` query (issue #101): the WHERE step's
/// pre-flight collision/writability check, computed against the *same* Backup-dir name
/// `backup()` will later compute (`backup_dir_name`) so a collision is learned here, not
/// mid-run.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhereCheckDto {
    pub location: String,
    /// The full Backup dir path this run would create — shown so the user can confirm where
    /// the Backup will land before launching.
    pub backup_dir: String,
}

impl From<&LocationSelection> for WhereCheckDto {
    fn from(resolved: &LocationSelection) -> Self {
        Self {
            location: resolved.location.to_string_lossy().into_owned(),
            backup_dir: resolved.backup_dir.to_string_lossy().into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
    use linux_backup_core::DirectoryState;

    use linux_backup_core::location::LocationSelection;

    use crate::encryption::EncryptionDecision;

    use super::{encryption_status, DirectoryStateDto, SelectionSummaryDto, WhereCheckDto};

    #[test]
    fn directory_state_dto_mirrors_each_core_variant() {
        assert_eq!(
            DirectoryStateDto::from(DirectoryState::All),
            DirectoryStateDto::All
        );
        assert_eq!(
            DirectoryStateDto::from(DirectoryState::Partial),
            DirectoryStateDto::Partial
        );
        assert_eq!(
            DirectoryStateDto::from(DirectoryState::None),
            DirectoryStateDto::None
        );
    }

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/nonexistent/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive,
        }
    }

    #[test]
    fn from_empty_selection_reports_is_empty_and_no_warnings() {
        let selection = SelectionModel::new(vec![]);

        let dto = SelectionSummaryDto::from(&selection);

        assert_eq!(dto.entry_count, 0);
        assert!(dto.is_empty);
        assert!(!dto.encryption_required);
        assert!(dto.warnings.is_empty());
    }

    #[test]
    fn from_selection_with_a_sensitive_entry_requires_encryption() {
        let selection = SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]);

        let dto = SelectionSummaryDto::from(&selection);

        assert_eq!(dto.entry_count, 1);
        assert!(!dto.is_empty);
        assert!(dto.encryption_required);
    }

    #[test]
    fn encryption_status_reflects_auto_insist_for_a_sensitive_selection() {
        let selection = SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]);
        let decision = EncryptionDecision::new(&selection);

        let dto = encryption_status(&selection, &decision);

        assert!(dto.auto_insist_required);
        assert_eq!(dto.sensitive_categories, vec!["SSH"]);
        assert!(!dto.has_passphrase);
        assert!(!dto.is_complete);
        assert!(!dto.is_invalidated);
    }

    #[test]
    fn encryption_status_is_complete_on_arrival_for_a_non_sensitive_selection() {
        let selection = SelectionModel::new(vec![entry("Shell", false, ".zshrc")]);
        let decision = EncryptionDecision::new(&selection);

        let dto = encryption_status(&selection, &decision);

        assert!(!dto.auto_insist_required);
        assert!(dto.is_complete);
        assert!(!dto.is_invalidated);
    }

    #[test]
    fn where_check_dto_carries_the_resolved_location_and_backup_dir() {
        let resolved = LocationSelection {
            location: PathBuf::from("/home/tester"),
            backup_dir: PathBuf::from("/home/tester/linux-backup-host-1"),
            warning: None,
        };

        let dto = WhereCheckDto::from(&resolved);

        assert_eq!(dto.location, "/home/tester");
        assert_eq!(dto.backup_dir, "/home/tester/linux-backup-host-1");
    }
}
