//! `linux-backup`'s TUI binary entry point.
//!
//! Per ADR-0001 ("tui is a thin view over core"), `main` itself only wires argv into a
//! [`cli::Command`] and dispatches to the matching command module — no backup/restore logic
//! lives here. Issue #29 is the first real command: `restore <dir> [--verify] [--overwrite]`
//! (`restore_cmd`). The `inquire` wizard (WHAT -> encryption -> WHERE -> REVIEW -> RUN) for
//! `backup` lands in later sub-issues of #17.
//!
//! `main` is one of the two places (tests being the other) AGENTS.md's coding conventions
//! exempt from the no-`unwrap`/`expect`/`panic!` rule — kept narrow here: the only thing this
//! function does with a `Result` is print it and pick an exit code, never `unwrap`/`expect`.

mod cli;
mod restore_cmd;
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
    }
}
