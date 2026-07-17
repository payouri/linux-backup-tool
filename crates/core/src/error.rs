//! Typed error surface for `core`, per the parent spec (#17): a `CoreError` built on
//! `thiserror` rather than `anyhow` — `anyhow` is reserved for `crates/tui`. Later issues
//! will grow this with variants for scan/crypto/archive/restore/profile failures (e.g. the
//! backup-fails-fast vs. restore-is-best-effort asymmetry from ADR-0002).

use thiserror::Error;

/// Placeholder error type for `core`. Variants will be added as each module (scan,
/// crypto, archive, restore, profile, ...) grows real fallible behavior.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),
}
