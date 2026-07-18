//! The concrete tar -> zstd -> age Archive pipeline (age outermost on disk).
//!
//! Per ADR-0001, this is deliberately a concrete implementation with no `ArchiveBackend`
//! trait — that abstraction waits for v1's real second (dedup-store) implementation.
//!
//! #19 built the real tar -> zstd pipeline with [`Encryption::Passphrase`] shaping the two
//! code paths (encrypted/unencrypted) as an identity pass-through stub. #23 replaces
//! [`encrypt`]/[`decrypt`]'s bodies with real `age::Encryptor::with_user_passphrase`
//! whole-stream passphrase encryption, without changing this module's public shape (still
//! `Result`-returning, still called the same way from `lib.rs`/`restore.rs`) — exactly
//! what #19's review anticipated.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use secrecy::{ExposeSecret, SecretString};
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
///
/// Deliberately does not derive `PartialEq`/`Eq`: `secrecy::SecretString` carries none
/// (comparing secrets by value invites timing side-channels and isn't a real use case
/// here — nothing in this codebase needs to compare two `Encryption` values for equality).
#[derive(Debug, Clone)]
pub enum Encryption {
    /// No encryption: Archive and Manifest bytes are written as plaintext.
    None,
    /// Whole-archive passphrase encryption via `age::Encryptor::with_user_passphrase`.
    /// Carried as a `secrecy::SecretString` rather than a plain `String` so the
    /// passphrase is zeroized on drop and redacted from `Debug` output everywhere it
    /// travels, not just at the `age` API boundary.
    Passphrase(SecretString),
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
#[derive(Debug)]
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

        // Same NotFound-vs-fatal split as the `fs::read` above: the file can vanish in the
        // narrow window between the two calls just as easily as before the first one, and
        // ADR-0002 draws the Vanished/fatal line on ENOENT itself, not on which particular
        // syscall observed it.
        let metadata = match fs::metadata(&entry.absolute_path) {
            Ok(metadata) => metadata,
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

/// Encrypts `plaintext` per `encryption` (a no-op for [`Encryption::None`]). Story 21: a
/// blank passphrase is a hard reject here — the point where a passphrase is actually used
/// to encrypt — rather than only advisorily flagged by `crypto::passphrase_strength`.
pub fn encrypt(plaintext: &[u8], encryption: &Encryption) -> Result<Vec<u8>, CoreError> {
    match encryption {
        Encryption::None => Ok(plaintext.to_vec()),
        Encryption::Passphrase(passphrase) => encrypt_with_passphrase(plaintext, passphrase),
    }
}

fn encrypt_with_passphrase(
    plaintext: &[u8],
    passphrase: &SecretString,
) -> Result<Vec<u8>, CoreError> {
    if passphrase.expose_secret().is_empty() {
        return Err(CoreError::BlankPassphrase);
    }

    let encryptor = age::Encryptor::with_user_passphrase(passphrase.clone());
    let mut writer = encryptor
        .wrap_output(Vec::new())
        .map_err(|source| CoreError::EncryptionFailed(source.to_string()))?;
    writer
        .write_all(plaintext)
        .map_err(|source| CoreError::EncryptionFailed(source.to_string()))?;
    // `StreamWriter::finish` writes the authentication tag; skipping it (e.g. dropping the
    // writer instead) silently produces a truncated file that will never decrypt — see
    // docs/research/rust-crates.md's age friction call-out.
    writer
        .finish()
        .map_err(|source| CoreError::EncryptionFailed(source.to_string()))
}

/// Decrypts `ciphertext`, the inverse of [`encrypt`]. `encrypted` mirrors whether the
/// source file carried the `.age` suffix (the authoritative on-disk signal, per
/// CONTEXT.md's Manifest entry) — when `false`, `ciphertext` is returned unchanged and
/// `passphrase` is ignored. When `true`, `passphrase` is required: a wrong passphrase or
/// corrupted/truncated ciphertext both surface as [`CoreError::DecryptionFailed`], never a
/// panic.
pub fn decrypt(
    ciphertext: &[u8],
    encrypted: bool,
    passphrase: Option<&SecretString>,
) -> Result<Vec<u8>, CoreError> {
    match (encrypted, passphrase) {
        (false, _) => Ok(ciphertext.to_vec()),
        (true, Some(passphrase)) => decrypt_with_passphrase(ciphertext, passphrase),
        (true, None) => Err(CoreError::DecryptionFailed(
            "backup is encrypted but no passphrase was supplied".to_string(),
        )),
    }
}

fn decrypt_with_passphrase(
    ciphertext: &[u8],
    passphrase: &SecretString,
) -> Result<Vec<u8>, CoreError> {
    let decryptor = age::Decryptor::new_buffered(ciphertext)
        .map_err(|source| CoreError::DecryptionFailed(source.to_string()))?;
    let identity = age::scrypt::Identity::new(passphrase.clone());

    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|source| CoreError::DecryptionFailed(source.to_string()))?;

    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|source| CoreError::DecryptionFailed(source.to_string()))?;
    Ok(plaintext)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntryRoot;

    fn entry(absolute_path: std::path::PathBuf, sensitive: bool) -> Entry {
        Entry {
            absolute_path,
            relative_path: std::path::PathBuf::from("thing"),
            root: EntryRoot::Home,
            category: "Test",
            sensitive,
        }
    }

    #[test]
    fn vanished_entry_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist");

        let built = build(&[entry(missing, false)]).expect("a Vanished entry must not abort");

        assert_eq!(built.vanished.len(), 1);
        assert!(built.checksums.is_empty());
    }

    #[test]
    fn vanished_sensitive_entry_is_recorded_as_sensitive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("gone-key");

        let built = build(&[entry(missing, true)]).expect("build should succeed");

        assert_eq!(built.vanished.len(), 1);
        assert!(built.vanished[0].sensitive);
    }

    #[test]
    fn real_read_error_aborts_the_build() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory can't be read as file bytes: `fs::read` fails with something other
        // than `NotFound` (e.g. `IsADirectory`) — exactly the "real read error" ADR-0002
        // says is fatal. Deterministic and portable, unlike a permission-denied probe that
        // a root-run test suite would sail straight through.
        let result = build(&[entry(tmp.path().to_path_buf(), false)]);

        match result {
            Err(CoreError::ReadFailed { .. }) => {}
            other => panic!("expected CoreError::ReadFailed, got {other:?}"),
        }
    }

    #[test]
    fn unencrypted_round_trip_is_a_pure_pass_through() {
        let plaintext = b"hello, plaintext".to_vec();
        let ciphertext = encrypt(&plaintext, &Encryption::None).expect("encrypt should succeed");
        assert_eq!(ciphertext, plaintext);

        let decrypted =
            decrypt(&ciphertext, false, None).expect("unencrypted decrypt should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn real_age_round_trip_with_the_correct_passphrase_recovers_the_plaintext() {
        let plaintext = b"the entire point of this exercise".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));

        let ciphertext = encrypt(&plaintext, &encryption).expect("encrypt should succeed");
        // Real age ciphertext must not equal the plaintext, else this test would pass even
        // if `encrypt` regressed back to an identity stub.
        assert_ne!(ciphertext, plaintext);

        let passphrase = SecretString::from("correct-horse-battery-staple");
        let decrypted = decrypt(&ciphertext, true, Some(&passphrase))
            .expect("decrypt with the correct passphrase should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypting_the_same_plaintext_twice_with_the_same_passphrase_yields_distinct_ciphertext() {
        // age's scrypt recipient draws a fresh random salt (and the STREAM layer a fresh
        // nonce) per call to `wrap_output` — encrypting identical input under an identical
        // passphrase twice must never produce identical bytes. Equal output here would mean
        // a fixed/reused salt or nonce, which would make the ciphertext leak equality
        // information and weaken the scheme against precomputation.
        let plaintext = b"the entire point of this exercise".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));

        let first = encrypt(&plaintext, &encryption).expect("first encrypt should succeed");
        let second = encrypt(&plaintext, &encryption).expect("second encrypt should succeed");

        assert_ne!(
            first, second,
            "encrypting identical plaintext under an identical passphrase twice must not \
             produce identical ciphertext bytes (would indicate a fixed salt/nonce)"
        );

        // Both must still independently decrypt back to the same plaintext.
        let passphrase = SecretString::from("correct-horse-battery-staple");
        assert_eq!(
            decrypt(&first, true, Some(&passphrase)).expect("decrypt first should succeed"),
            plaintext
        );
        assert_eq!(
            decrypt(&second, true, Some(&passphrase)).expect("decrypt second should succeed"),
            plaintext
        );
    }

    #[test]
    fn decrypt_with_the_wrong_passphrase_fails_typed_not_panicking() {
        let plaintext = b"sensitive bytes".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));
        let ciphertext = encrypt(&plaintext, &encryption).expect("encrypt should succeed");

        let wrong_passphrase = SecretString::from("a-completely-different-passphrase");
        match decrypt(&ciphertext, true, Some(&wrong_passphrase)) {
            Err(CoreError::DecryptionFailed(_)) => {}
            other => panic!("expected CoreError::DecryptionFailed, got {other:?}"),
        }
    }

    #[test]
    fn decrypt_of_corrupted_ciphertext_fails_typed_not_panicking() {
        let plaintext = b"sensitive bytes".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));
        let mut ciphertext = encrypt(&plaintext, &encryption).expect("encrypt should succeed");
        // Flip a byte well past the age header so this exercises stream-body corruption,
        // not just header parsing.
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xFF;

        let passphrase = SecretString::from("correct-horse-battery-staple");
        match decrypt(&ciphertext, true, Some(&passphrase)) {
            Err(CoreError::DecryptionFailed(_)) => {}
            other => panic!("expected CoreError::DecryptionFailed, got {other:?}"),
        }
    }

    #[test]
    fn decrypting_an_encrypted_backup_without_a_passphrase_fails_typed() {
        let plaintext = b"sensitive bytes".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from("correct-horse-battery-staple"));
        let ciphertext = encrypt(&plaintext, &encryption).expect("encrypt should succeed");

        match decrypt(&ciphertext, true, None) {
            Err(CoreError::DecryptionFailed(_)) => {}
            other => panic!("expected CoreError::DecryptionFailed, got {other:?}"),
        }
    }

    #[test]
    fn blank_passphrase_is_rejected_at_encrypt_time() {
        let plaintext = b"anything".to_vec();
        let encryption = Encryption::Passphrase(SecretString::from(""));

        match encrypt(&plaintext, &encryption) {
            Err(CoreError::BlankPassphrase) => {}
            other => panic!("expected CoreError::BlankPassphrase, got {other:?}"),
        }
    }
}
