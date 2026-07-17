//! Whole-archive passphrase encryption via `age::Encryptor::with_user_passphrase` and
//! `secrecy::SecretString`, plus the Auto-insist policy (does this Selection contain
//! anything Sensitive -> is encryption required).
//!
//! Auto-insist policy is pure logic over a `SelectionModel` and is on-seam / unit-tested;
//! the interactive passphrase prompt itself is off-seam (lives in `crates/tui`).
