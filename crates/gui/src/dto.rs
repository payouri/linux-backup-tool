//! View DTOs crossing the Tauri IPC boundary, built from `core::SelectionModel`'s plain
//! accessors via `From` (ADR-0008 Decision 2). `core` gains zero net-new `serde` derives —
//! serialization lives entirely here, in `gui`.

use linux_backup_core::{DirectoryState, SelectionModel};

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use linux_backup_core::model::{Entry, EntryRoot, EntryType, SelectionModel};
    use linux_backup_core::DirectoryState;

    use super::{DirectoryStateDto, SelectionSummaryDto};

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
}
