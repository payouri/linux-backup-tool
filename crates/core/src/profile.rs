//! Load/save/prune of `~/.config/<app>/profile.toml` (`serde` + `toml`): last Selection
//! (Categories, Custom paths, Drill-down deselections) and last Output location.
//!
//! Stale paths are pruned on load (drop-with-note, never error). Encryption state is
//! never persisted.
