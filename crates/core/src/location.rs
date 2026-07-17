//! Output location enumeration: mounted drives via `sysinfo`, the `lsblk`/`udisksctl`
//! delta for unmounted USB partitions, and the always-offered `$HOME` row.
//!
//! Per issue #17's Testing Decisions, this system-boundary edge (real hardware/mount
//! state) stays off the round-trip seam and is not unit-tested in the alpha.
