//! The `manifest.json` schema and (de)serialization via `serde`/`serde_json`: schema/tool
//! version, `created_at`/`host`/`user`/`uid`/`home`/`os` (from `RunContext`), the selected
//! Entries (root, root-tag, Category, sensitivity), a per-file SHA-256 checksum map, a
//! top-level `archive_sha256`, the `vanished` array, and per-manager Package lists.
//!
//! Package lists (`packages`) are captured by the `packages` module (#25) and passed in
//! through [`ManifestInputs`] — this module just carries the shape, agnostic to how the
//! per-manager entries were gathered. No POSIX metadata table here by design: mode/uid/gid
//! /symlink live in the tar stream (see `archive`), not duplicated in the Manifest, so the
//! two can't drift out of sync.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::context::RunContext;
use crate::model::{Entry, EntryRoot};

/// The Manifest schema version this build of `core` writes and can read. Bump alongside
/// any breaking change to the fields below.
pub const SCHEMA_VERSION: u32 = 1;

/// The `manifest.json` schema (`manifest.json.age` sidecar name when the Archive is
/// encrypted — the suffix, not this struct, is the authoritative encryption signal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub tool_version: String,
    /// UTC RFC 3339 timestamp, from `RunContext.now` — never the wall clock (ADR-0004).
    pub created_at: String,
    pub host: String,
    pub user: String,
    pub uid: u32,
    pub home: PathBuf,
    pub os: String,
    pub entries: Vec<ManifestEntry>,
    /// Path (Archive-internal, `home/...` or `absolute/...`) -> SHA-256 hex of the
    /// plaintext file bytes.
    pub checksums: BTreeMap<String, String>,
    /// SHA-256 hex of the plaintext (pre-encryption) `.tar.zst` Archive bytes. Restore
    /// verifies this before untarring (ADR-0002's pre-flight abort condition).
    pub archive_sha256: String,
    pub vanished: Vec<VanishedEntry>,
    /// Per-manager Package lists (pacman, aur, flatpak, snap, cargo, npm — see
    /// `packages::capture_all`), keyed by manager name. Empty when the manager didn't run
    /// (e.g. a fake/`NoCommandRunner` in tests) or when nothing was captured.
    #[serde(default)]
    pub packages: BTreeMap<String, Vec<PackageInfo>>,
}

/// The Manifest's `root` tag on an Entry — mirrors [`EntryRoot`] but is the
/// serde-serializable, on-disk form of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestRoot {
    Home,
    Absolute,
}

impl From<EntryRoot> for ManifestRoot {
    fn from(root: EntryRoot) -> Self {
        match root {
            EntryRoot::Home => ManifestRoot::Home,
            EntryRoot::Absolute => ManifestRoot::Absolute,
        }
    }
}

/// One selected root as recorded in the Manifest: `path`, `root` tag, Category, and
/// sensitivity — no POSIX metadata (that lives in the tar stream).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub root: ManifestRoot,
    pub category: String,
    pub sensitive: bool,
}

impl From<&Entry> for ManifestEntry {
    fn from(entry: &Entry) -> Self {
        Self {
            path: entry.relative_path.to_string_lossy().into_owned(),
            root: entry.root.into(),
            category: entry.category.to_string(),
            sensitive: entry.sensitive,
        }
    }
}

/// A benign ENOENT-after-scan Entry (CONTEXT.md's "Vanished"): skipped-with-note, never
/// fatal to the run (ADR-0002).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VanishedEntry {
    pub path: String,
    pub sensitive: bool,
}

/// One package record within a Package list.
///
/// `version` is informational only, per story 48 — nothing in this crate reads it back to
/// pin a reinstall (there is deliberately no "pin_version" field or similar); reinstalling
/// on a rolling-release distro must not chase an unavailable exact version. `origin` (e.g.
/// flatpak's remote) and `channel` (e.g. snap's tracking channel) are per-manager extras;
/// most managers leave both `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// The pieces a completed backup run assembles into a [`Manifest`].
pub struct ManifestInputs<'a> {
    pub ctx: &'a RunContext,
    pub tool_version: &'a str,
    pub entries: &'a [Entry],
    pub checksums: BTreeMap<String, String>,
    pub archive_sha256: String,
    pub vanished: Vec<VanishedEntry>,
    /// Per-manager Package lists, already captured (see `packages::capture_all`) — this
    /// module just carries them into the Manifest, agnostic to how they were gathered.
    pub packages: BTreeMap<String, Vec<PackageInfo>>,
}

/// Assembles a [`Manifest`] from a completed backup run's pieces.
pub fn build(inputs: ManifestInputs<'_>) -> Manifest {
    Manifest {
        schema_version: SCHEMA_VERSION,
        tool_version: inputs.tool_version.to_string(),
        created_at: format_rfc3339(inputs.ctx.now),
        host: inputs.ctx.host.clone(),
        user: inputs.ctx.user.clone(),
        uid: inputs.ctx.uid,
        home: inputs.ctx.home.clone(),
        os: inputs.ctx.os.clone(),
        entries: inputs.entries.iter().map(ManifestEntry::from).collect(),
        checksums: inputs.checksums,
        archive_sha256: inputs.archive_sha256,
        vanished: inputs.vanished,
        packages: inputs.packages,
    }
}

/// Formats a [`SystemTime`] as a UTC RFC 3339 timestamp (e.g. `2024-01-02T03:04:05Z`).
///
/// Deliberately dependency-free (no `chrono`/`time` crate): this is purely for the
/// Manifest's self-identifying `created_at` field (CONTEXT.md), never parsed back by
/// anything in the alpha, so a minimal calendar conversion is enough.
pub fn format_rfc3339(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (time_of_day / 3600, (time_of_day / 60) % 60, time_of_day % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days-since-epoch (1970-01-01) -> (year, month,
/// day). A small, well-known, dependency-free algorithm — see
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rfc3339_epoch() {
        assert_eq!(format_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn format_rfc3339_known_date() {
        // 2024-01-02T03:04:05Z, per date -u -d @1704164645
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_704_164_645);
        assert_eq!(format_rfc3339(t), "2024-01-02T03:04:05Z");
    }
}
