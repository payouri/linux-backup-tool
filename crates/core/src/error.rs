//! Typed error surface for `core`, per the parent spec (#17): a `CoreError` built on
//! `thiserror` rather than `anyhow` — `anyhow` is reserved for `crates/tui`. Later issues
//! will grow this with variants for scan/crypto/archive/restore/profile failures (e.g. the
//! backup-fails-fast vs. restore-is-best-effort asymmetry from ADR-0002).

use std::path::PathBuf;

use thiserror::Error;

/// `core`'s single error type. Variants will keep growing as each module (scan, crypto,
/// archive, restore, profile, location, ...) grows real fallible behavior.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),

    /// The target Backup dir already exists. Per story 33 / ADR "collision unit", a
    /// backup run must refuse rather than silently overwrite a previous one.
    #[error("backup dir already exists: {0}")]
    BackupDirExists(PathBuf),

    /// An Output location that isn't a writable directory (story 31: hard-refuse).
    #[error("output location {0} is not writable")]
    LocationNotWritable(PathBuf),

    /// An Output location path that doesn't exist and isn't a directory (e.g. a file at
    /// that path, or nothing at all).
    #[error("output location {0} does not exist or is not a directory")]
    LocationNotFound(PathBuf),

    /// A real read error while scanning or reading an Entry's bytes during backup
    /// (permission-denied, I/O error, device-gone). Per ADR-0002 this is fatal: the run
    /// aborts and the freshly-created Backup dir is deleted. Distinct from a Vanished
    /// (ENOENT) entry, which is benign and does not produce this variant.
    #[error("failed to read {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Any other I/O failure that isn't specifically one of the above (creating the
    /// Backup dir, writing the archive/manifest files, cleaning up on failure, ...).
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Manifest (de)serialization failed.
    #[error("manifest (de)serialization failed: {0}")]
    Manifest(#[from] serde_json::Error),

    /// The Backup dir's Manifest could not be found or read during restore.
    #[error("manifest not found or unreadable in backup dir {0}")]
    ManifestMissing(PathBuf),

    /// The Archive's checksum did not match `manifest.archive_sha256`. Per ADR-0002 this
    /// is the one pre-flight condition that aborts a restore before any file is touched.
    #[error("archive checksum mismatch: expected {expected}, got {actual}")]
    ArchiveChecksumMismatch { expected: String, actual: String },

    /// The Archive could not be built (tar/zstd pipeline failure) during backup.
    #[error("failed to build archive: {0}")]
    ArchiveBuildFailed(String),

    /// Encryption of the Archive or Manifest bytes failed. A no-op today (see
    /// `archive::encrypt`'s #19 stub), but real `age` encryption (#23) is fallible, so the
    /// signature carries `Result` from the start rather than forcing every call site to
    /// change shape when the stub is replaced.
    #[error("failed to encrypt: {0}")]
    EncryptionFailed(String),

    /// Decryption of the Archive or Manifest bytes failed (e.g. a wrong passphrase once
    /// #23 lands real `age` decryption). A no-op today (see `archive::decrypt`'s #19
    /// stub), carried as `Result` for the same reason as `EncryptionFailed`.
    #[error("failed to decrypt: {0}")]
    DecryptionFailed(String),

    /// The real `lsblk` subprocess (story 34's unmounted-USB delta) exited non-zero.
    /// Distinct from `LsblkParseFailed`: this is the subprocess itself failing, not its
    /// (malformed) output.
    #[error("lsblk failed: {0}")]
    LsblkCommandFailed(String),

    /// `lsblk --json -b` produced output the parser couldn't read as the expected shape.
    /// Kept as a message rather than `#[from] serde_json::Error` because `Manifest`
    /// already claims that conversion and a type can only implement `From` once.
    #[error("failed to parse lsblk output: {0}")]
    LsblkParseFailed(String),
}
