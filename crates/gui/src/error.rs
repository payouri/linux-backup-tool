//! `SerializableCoreError`: the one error type that crosses IPC (ADR-0008 Decisions 2/3).
//!
//! `core::CoreError` is `thiserror`-based and deliberately not `Serialize` (ADR-0008
//! rejects deriving `Serialize` on `core` types outright). Every fallible command's `Err`
//! and `RunOutcome::Failed` funnel through this single lossy flattening instead:
//! `PathBuf`/`std::io::Error` are already folded into readable text by `CoreError`'s own
//! `Display` impl (thiserror's `#[error(...)]` messages), so `message` is just that
//! rendered string; `kind` is a stable, machine-matchable tag the frontend can switch on
//! (e.g. to special-case `blank_passphrase`) without parsing prose.

use linux_backup_core::CoreError;

/// The lossy, wire-safe shape of a [`CoreError`]. See the module doc for why a flattened
/// `{kind, message}` pair — rather than mirroring `CoreError`'s variants and fields onto the
/// wire — is the whole of what crosses IPC.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializableCoreError {
    pub kind: &'static str,
    pub message: String,
}

impl From<&CoreError> for SerializableCoreError {
    fn from(error: &CoreError) -> Self {
        Self {
            kind: kind_tag(error),
            message: error.to_string(),
        }
    }
}

/// A stable tag per [`CoreError`] variant. Matched exhaustively (no catch-all `_`) so a new
/// `CoreError` variant fails this build rather than silently serializing as an
/// unrecognizable message-only error.
fn kind_tag(error: &CoreError) -> &'static str {
    match error {
        CoreError::NotImplemented(_) => "not_implemented",
        // A cancelled backup is rendered neutrally, never as a failure (ADR-0008 Decision 3:
        // `RunOutcome::Cancelled`, not `Failed`) — this tag exists only so
        // `SerializableCoreError::from` stays exhaustive over `CoreError` should a caller
        // ever flatten a `Cancelled` through the generic error path instead of the
        // `RunOutcome` mapping.
        CoreError::Cancelled => "cancelled",
        CoreError::BackupDirExists(_) => "backup_dir_exists",
        CoreError::EmptySelection => "empty_selection",
        CoreError::LocationNotWritable(_) => "location_not_writable",
        CoreError::LocationNotFound(_) => "location_not_found",
        CoreError::ReadFailed { .. } => "read_failed",
        CoreError::Io { .. } => "io",
        CoreError::Manifest(_) => "manifest",
        CoreError::Profile(_) => "profile",
        CoreError::ManifestMissing(_) => "manifest_missing",
        CoreError::ArchiveChecksumMismatch { .. } => "archive_checksum_mismatch",
        CoreError::ArchiveBuildFailed(_) => "archive_build_failed",
        CoreError::EncryptionFailed(_) => "encryption_failed",
        CoreError::DecryptionFailed(_) => "decryption_failed",
        CoreError::BlankPassphrase => "blank_passphrase",
        CoreError::LsblkCommandFailed(_) => "lsblk_command_failed",
        CoreError::LsblkParseFailed(_) => "lsblk_parse_failed",
        CoreError::CustomPathOutsideHome { .. } => "custom_path_outside_home",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use linux_backup_core::CoreError;

    use super::SerializableCoreError;

    #[test]
    fn blank_passphrase_flattens_to_its_kind_and_message() {
        let error = CoreError::BlankPassphrase;

        let serializable = SerializableCoreError::from(&error);

        assert_eq!(serializable.kind, "blank_passphrase");
        assert_eq!(serializable.message, "passphrase must not be blank");
    }

    #[test]
    fn a_path_bearing_variant_flattens_its_pathbuf_to_a_string_message() {
        let error = CoreError::BackupDirExists(PathBuf::from("/home/tester/linux-backup-x-1"));

        let serializable = SerializableCoreError::from(&error);

        assert_eq!(serializable.kind, "backup_dir_exists");
        assert_eq!(
            serializable.message,
            "backup dir already exists: /home/tester/linux-backup-x-1"
        );
    }

    #[test]
    fn an_io_error_flattens_to_its_display_message() {
        let source = std::io::Error::other("permission denied");
        let error = CoreError::Io {
            path: PathBuf::from("/etc/shadow"),
            source,
        };

        let serializable = SerializableCoreError::from(&error);

        assert_eq!(serializable.kind, "io");
        assert_eq!(
            serializable.message,
            "I/O error at /etc/shadow: permission denied"
        );
    }

    #[test]
    fn cancelled_flattens_to_its_kind_and_message() {
        let error = CoreError::Cancelled;

        let serializable = SerializableCoreError::from(&error);

        assert_eq!(serializable.kind, "cancelled");
        assert_eq!(serializable.message, "run cancelled");
    }
}
