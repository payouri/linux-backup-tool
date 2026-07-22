//! Builds a real [`RunContext`] from the actual machine: `$HOME`, hostname, uid/euid-based
//! [`Privilege`], OS identifier, and the wall clock.
//!
//! Per ADR-0004, `core` reads nothing ambient — this is exactly the kind of read `gui`
//! exists to do instead, the same way `tui/src/system.rs` does for the ratatui binary.
//! ADR-0008 explicitly rules out `gui` and `tui` sharing code directly (a `crates/ui-common`
//! is created only if real duplication later appears, per ADR-0001), so this module
//! deliberately duplicates that one rather than importing it.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use linux_backup_core::{Privilege, RunContext};

/// Builds a [`RunContext`] from the real machine for the GUI's managed selection state.
///
/// `output_location` is set to `home`: the walking skeleton (#99) has no WHERE step yet
/// (that's a later build-sequencing slice, per #97), so nothing reads this field for
/// today's one command — it's populated with a harmless, always-available value rather
/// than left to construct a `RunContext` with a dummy/placeholder path (mirrors
/// `tui/src/system.rs::build_run_context`'s identical note for its pre-WHERE-step commands).
pub fn build_run_context() -> Result<RunContext> {
    let home = real_home()?;
    let uid = real_uid();
    let privilege = if is_effective_root() {
        Privilege::Root
    } else {
        Privilege::NonRoot
    };

    Ok(RunContext {
        home: home.clone(),
        output_location: home,
        privilege,
        host: real_hostname(),
        user: real_user(),
        uid,
        os: real_os_identifier(),
        now: SystemTime::now(),
    })
}

fn real_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("$HOME is not set; linux-backup needs a real home directory")
}

fn real_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown-user".to_string())
}

/// The real uid (not euid): what gets recorded in the Manifest as "who ran this",
/// independent of the euid-based privilege check below.
fn real_uid() -> u32 {
    // SAFETY: `getuid()` takes no arguments, performs no pointer dereferences, and cannot
    // fail per POSIX (it's defined to always succeed).
    unsafe { libc::getuid() }
}

/// Whether this process's *effective* uid is root — the check that governs whether the
/// real OS will actually let `core::restore`'s `chown` calls succeed (see `RunContext`'s
/// `Privilege` doc: it "does not change what the real OS lets the real process do").
fn is_effective_root() -> bool {
    // SAFETY: same as `real_uid` above — `geteuid()` is argument-free and infallible.
    unsafe { libc::geteuid() == 0 }
}

/// The real hostname via `gethostname(2)`, falling back to a placeholder on any failure
/// (oversized buffer, non-UTF8 name, syscall error) rather than aborting the whole run over
/// a cosmetic Manifest field.
fn real_hostname() -> String {
    let mut buf = vec![0u8; 256];
    // SAFETY: `buf` is a valid, writable buffer of `buf.len()` bytes, and `buf.len()` is
    // passed accurately as the buffer size — exactly what `gethostname(2)` requires.
    let result = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if result != 0 {
        return "unknown-host".to_string();
    }
    let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..nul_pos].to_vec()).unwrap_or_else(|_| "unknown-host".to_string())
}

/// A short OS identifier for the Manifest's `os` field. Prefers `/etc/os-release`'s
/// `PRETTY_NAME` (e.g. "Manjaro Linux") since that's what a human reads back years later;
/// falls back to `std::env::consts::OS` (e.g. "linux") if the file is missing or
/// unparsable, which is still a real, if less specific, fact rather than a dummy value.
fn real_os_identifier() -> String {
    if let Ok(contents) = fs::read_to_string("/etc/os-release") {
        for line in contents.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                return value.trim_matches('"').to_string();
            }
        }
    }
    std::env::consts::OS.to_string()
}
