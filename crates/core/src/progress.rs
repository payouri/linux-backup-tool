//! Progress reporting: a callback trait `tui` implements to drive per-phase progress bars.
//!
//! `core` stays synchronous with no async runtime — any channel-based adapter (for a
//! future concurrent UI) lives in `crates/tui`, not here.
