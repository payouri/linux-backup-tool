//! The concrete tar -> zstd -> age Archive pipeline (age outermost on disk).
//!
//! Per ADR-0001, this is deliberately a concrete implementation with no `ArchiveBackend`
//! trait — that abstraction waits for v1's real second (dedup-store) implementation.
//!
//! #19 scope: a real tar -> zstd pipeline. [`Encryption::Passphrase`] exists so the two
//! code paths (encrypted/unencrypted) already shape the round-trip seam, but
//! [`encrypt`]/[`decrypt`] are today an identity pass-through stub — #23 will swap in real
//! `age::Encryptor::with_user_passphrase` encryption there without changing this module's
//! public shape or its callers.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::CoreError;
use crate::model::Entry;

/// The Archive's filename within a Backup dir, before any encryption suffix.
pub const ARCHIVE_FILENAME: &str = "archive.tar.zst";
/// The Manifest's filename within a Backup dir, before any encryption suffix.
pub const MANIFEST_FILENAME: &str = "manifest.json";
/// Suffix appended to both the Archive and Manifest filenames when encrypted (CONTEXT.md:
/// "Mirrors the Archive's encryption all-or-nothing").
pub const ENCRYPTED_SUFFIX: &str = ".age";

/// Whole-archive encryption choice. All-or-nothing across the Archive + Manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encryption {
    /// No encryption: Archive and Manifest bytes are written as plaintext.
    None,
    /// Whole-archive passphrase encryption.
    ///
    /// STUB for #19: `encrypt`/`decrypt` are an identity pass-through today — the
    /// passphrase is carried but not yet used. #23 replaces the internals with real
    /// `age` encryption; this variant's shape (and every call site) stays the same.
    Passphrase(String),
}

impl Encryption {
    /// Whether this choice puts the `.age` suffix on the Archive/Manifest filenames.
    pub fn is_encrypted(&self) -> bool {
        matches!(self, Encryption::Passphrase(_))
    }
}

/// One Entry that vanished (ENOENT) between scan and archive-build time — recorded rather
/// than aborting the run, per ADR-0002.
#[derive(Debug, Clone)]
pub struct VanishedDuringArchive {
    /// The Entry's Archive-internal path (`entry.archive_path()`), for correlating with
    /// the eventual Manifest's `vanished` array.
    pub archive_path: String,
    pub sensitive: bool,
}

/// The in-memory result of building an Archive.
pub struct BuiltArchive {
    /// The plaintext (pre-encryption) `.tar.zst` bytes.
    pub plaintext_bytes: Vec<u8>,
    /// Archive-internal path -> SHA-256 hex of that file's plaintext bytes, computed from
    /// the same read used to write the tar entry (one filesystem read per file).
    pub checksums: BTreeMap<String, String>,
    pub vanished: Vec<VanishedDuringArchive>,
}

/// Builds the tar -> zstd Archive from `entries`.
///
/// A Vanished entry (ENOENT at read time) is skipped-with-note; any other read error
/// aborts immediately with [`CoreError::ReadFailed`], per ADR-0002's backup fail-fast
/// policy. The caller is responsible for deleting the just-created Backup dir on `Err`.
pub fn build(entries: &[Entry]) -> Result<BuiltArchive, CoreError> {
    let mut checksums = BTreeMap::new();
    let mut vanished = Vec::new();

    let zstd_encoder = zstd::stream::write::Encoder::new(Vec::new(), 0)
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    let mut tar_builder = tar::Builder::new(zstd_encoder);

    for entry in entries {
        let archive_path = entry.archive_path();
        let archive_path_str = archive_path.to_string_lossy().into_owned();

        let bytes = match fs::read(&entry.absolute_path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                vanished.push(VanishedDuringArchive {
                    archive_path: archive_path_str,
                    sensitive: entry.sensitive,
                });
                continue;
            }
            Err(source) => {
                return Err(CoreError::ReadFailed {
                    path: entry.absolute_path.clone(),
                    source,
                });
            }
        };

        let metadata = fs::metadata(&entry.absolute_path).map_err(|source| CoreError::ReadFailed {
            path: entry.absolute_path.clone(),
            source,
        })?;

        checksums.insert(archive_path_str, sha256_hex(&bytes));

        let mut header = tar::Header::new_gnu();
        header.set_metadata(&metadata);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, &archive_path, bytes.as_slice())
            .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    }

    let zstd_encoder = tar_builder
        .into_inner()
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;
    let plaintext_bytes = zstd_encoder
        .finish()
        .map_err(|e| CoreError::ArchiveBuildFailed(e.to_string()))?;

    Ok(BuiltArchive {
        plaintext_bytes,
        checksums,
        vanished,
    })
}

/// SHA-256 of `bytes`, hex-encoded. Shared by per-file checksums and `archive_sha256` so
/// both are computed the same way.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// STUB for #19: identity pass-through — see the module doc. #23 replaces the *body* with
/// real `age::Encryptor::with_user_passphrase` encryption without changing the signature or
/// its callers. The `Result` is load-bearing even though this stub never fails: real `age`
/// encryption is fallible (e.g. an I/O error while wrapping the writer), so the signature
/// carries `Result` from the start rather than forcing every call site to change shape
/// (`= encrypt(..)` to `= encrypt(..)?`) the day the stub is replaced.
pub fn encrypt(plaintext: &[u8], _encryption: &Encryption) -> Result<Vec<u8>, CoreError> {
    Ok(plaintext.to_vec())
}

/// STUB for #19: identity pass-through, the inverse of [`encrypt`]. `encrypted` mirrors
/// whether the source file carried the `.age` suffix. See [`encrypt`]'s doc for why this
/// returns `Result` despite never failing today — real `age` decryption fails on a wrong
/// passphrase or corrupted ciphertext.
pub fn decrypt(ciphertext: &[u8], _encrypted: bool) -> Result<Vec<u8>, CoreError> {
    Ok(ciphertext.to_vec())
}

/// The Archive's filename within a Backup dir, given the run's encryption choice.
pub fn archive_filename(encryption: &Encryption) -> String {
    filename_for(ARCHIVE_FILENAME, encryption.is_encrypted())
}

/// The Manifest's filename within a Backup dir, given the run's encryption choice.
pub fn manifest_filename(encryption: &Encryption) -> String {
    filename_for(MANIFEST_FILENAME, encryption.is_encrypted())
}

/// `base` with the `.age` suffix appended iff `encrypted`. Shared by
/// `archive_filename`/`manifest_filename` (which know the choice ahead of time, at backup)
/// and `restore` (which discovers it from what's on disk).
pub fn filename_for(base: &str, encrypted: bool) -> String {
    if encrypted {
        format!("{base}{ENCRYPTED_SUFFIX}")
    } else {
        base.to_string()
    }
}

/// Writes already-built, already-"encrypted" (per [`encrypt`]) bytes to `path`.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    fs::write(path, bytes).map_err(|source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}
