//! `linux-backup`'s TUI binary entry point.
//!
//! Per ADR-0001 ("tui is a thin view over core"), `main` itself only wires argv into a
//! [`cli::Command`] and dispatches to the matching command module — no backup/restore logic
//! lives here. Issue #29 was the first real command: `restore <dir> [--verify]
//! [--overwrite]` (`restore_cmd`). Issue #30 adds `select` (`select_cmd`), the interactive
//! WHAT screen, standing alone as its own subcommand so it's runnable and verifiable ahead
//! of `backup` wiring it into the full WHAT -> encryption -> WHERE -> REVIEW -> RUN wizard
//! (#31/#32/…).
//!
//! `main` is one of the two places (tests being the other) AGENTS.md's coding conventions
//! exempt from the no-`unwrap`/`expect`/`panic!` rule — kept narrow here: the only thing this
//! function does with a `Result` is print it and pick an exit code, never `unwrap`/`expect`.

// `#[cfg(test)]` modules in this crate (`cli`, `restore_cmd`, `system`, `select_cmd`) compile
// under `--cfg test`, so this allow only ever applies to them — see core/src/lib.rs for the
// same pattern.
#![cfg_attr(test, allow(clippy::disallowed_methods, clippy::disallowed_macros))]

mod cli;
mod restore_cmd;
mod select_cmd;
mod system;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    // argv[0] is the program name; every parseable token starts after it.
    let args = std::env::args().skip(1);
    match cli::parse_args(args).map_err(anyhow::Error::msg)? {
        cli::Command::Restore(restore_args) => restore_cmd::run(&restore_args),
        cli::Command::Select => select_cmd::run(),
    }
}
