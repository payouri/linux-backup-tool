//! `linux-backup-gui`'s binary entry point. Mirrors `crates/tui/src/main.rs`'s shape: `main`
//! stays a thin `Result`-printing shell (one of the two places, tests being the other,
//! AGENTS.md's coding conventions exempt from the no-`unwrap`/`expect`/`panic!` rule) and
//! every real decision lives in the library (`linux_backup_gui::run`), so it's reachable
//! from `crates/gui/tests/` too.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match linux_backup_gui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
