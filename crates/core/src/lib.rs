//! `core`: linux-backup's UI-free domain library.
//!
//! Owns the Selection model and all backup/restore logic. Per ADR-0001, this crate's
//! Cargo.toml must never depend on `inquire`, `ratatui`, or any other terminal/rendering
//! crate — the core/tui split is compiler-enforced, not just a convention.
//!
//! Per ADR-0004, `core` reads nothing from the ambient system (no `std::env`, `dirs`,
//! `hostname`, `getuid`, or clock calls anywhere in this crate). Every run fact it needs
//! arrives through an injected [`context::RunContext`], filled in by `crates/tui`'s `main`
//! from the real machine, or by tests with fixed values.

pub mod archive;
pub mod catalog;
pub mod context;
pub mod crypto;
pub mod error;
pub mod location;
pub mod manifest;
pub mod model;
pub mod profile;
pub mod progress;
pub mod restore;
pub mod scan;

pub use context::{Privilege, RunContext};
pub use error::CoreError;
