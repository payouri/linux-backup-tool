//! The concrete tar -> zstd -> age Archive pipeline (age outermost on disk).
//!
//! Per ADR-0001, this is deliberately a concrete implementation with no `ArchiveBackend`
//! trait — that abstraction waits for v1's real second (dedup-store) implementation.
