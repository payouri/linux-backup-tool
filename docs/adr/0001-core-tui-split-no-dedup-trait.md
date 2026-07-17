# Cargo workspace: `core` library + `tui` binary, with no archive-backend trait yet

The alpha is a Cargo workspace: `crates/core` is a UI-free library that owns the Selection model and all backup/restore logic, and `crates/tui` is a thin `inquire`-based binary over it. The core/UI boundary is compiler-enforced (core has zero UI dependencies) so a future `gui` or `dedup` crate can slot in beside `tui` without touching core.

Deliberately, `core::archive` is a **concrete** tar → zstd → age implementation with no backend trait — even though v1 will add a borg/restic-style dedup store. We are not abstracting the archive backend until that second implementation actually exists.

## Consequences

A reader will be tempted to introduce an `ArchiveBackend` trait immediately for "cleanliness." Don't — a one-implementation trait would be designed against a backend we haven't built and would almost certainly be the wrong shape. The trait waits for v1's real second impl.
