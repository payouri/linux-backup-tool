//! Untar-to-path restore with privilege-aware uid/gid handling and Re-rooting
//! (home-tagged Entries re-based under the restoring user's `$HOME`).
//!
//! Per ADR-0002, restore is best-effort: it continues past per-file failures and collects
//! a `path + reason` report, aborting only on a pre-flight `archive_sha256` mismatch.
