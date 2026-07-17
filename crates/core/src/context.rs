//! `RunContext`: the one struct through which every run fact enters `core`.
//!
//! Per ADR-0004, `core` reads nothing from the ambient system on its own — no
//! `std::env::var("HOME")`, no `dirs::home_dir()`, no `hostname`, no `getuid()`, no
//! `SystemTime::now()`. `crates/tui`'s `main` fills a `RunContext` from the real machine;
//! tests construct one with fixed values, which is what makes the round-trip test
//! (`crates/core/tests/roundtrip.rs`) fully deterministic.
//!
//! If a future module needs a new fact about the run, add a field here rather than
//! reaching for the environment or the clock inside `core`.

use std::path::PathBuf;
use std::time::SystemTime;

/// Whether the current run is executing as root.
///
/// This governs `core`'s uid/gid-restore *logic* only (story 53) — it does not change
/// what the real OS lets the real process do. See ADR-0004's "does not move" note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    Root,
    NonRoot,
}

/// Every ambient fact `core` needs for a single backup or restore run, injected up front.
///
/// Fields mirror ADR-0004 exactly: `home`, `output_location`, `privilege`, `host`, `user`,
/// `uid`, `os`, `now`. Several of these (`host`, `user`, `uid`, `os`, `now`) are not just
/// convenience — they land verbatim in the Manifest (`created_at`, `host`, `user`, `uid`,
/// `os`) and in the Backup dir name (`linux-backup-<host>-<timestamp>`), so core must
/// receive them rather than derive them, or the round-trip test could neither assert the
/// Manifest exactly nor stay deterministic across machines and runs.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// The user's home directory. Entries tagged `root: "home"` in the Manifest are
    /// resolved relative to this (see Re-rooting).
    pub home: PathBuf,
    /// The directory selected to hold the Backup dir (an Output location, never the
    /// Backup dir itself).
    pub output_location: PathBuf,
    /// Whether this run is executing as root; governs uid/gid-restore behavior.
    pub privilege: Privilege,
    /// Hostname, used in the Backup dir name and the Manifest.
    pub host: String,
    /// Username, recorded in the Manifest.
    pub user: String,
    /// Numeric uid, recorded in the Manifest.
    pub uid: u32,
    /// OS identifier (e.g. distro/kernel description), recorded in the Manifest.
    pub os: String,
    /// The instant this run started, used for `created_at` in the Manifest and the
    /// timestamp component of the Backup dir name.
    ///
    /// Note per ADR-0004: this sets the Manifest timestamp and dir name, but not the
    /// per-file mtimes the `tar` crate embeds from the filesystem — `archive_sha256`
    /// therefore stays non-deterministic across runs and is asserted as a property, never
    /// pinned literally, in the round-trip test.
    pub now: SystemTime,
}
