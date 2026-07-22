//! `linux-backup`'s TUI binary entry point.
//!
//! Per ADR-0005 / spec #62, the interactive surface is one ratatui `App` reducer driven by a
//! single shared runtime seeded with an initial [`Screen`]. `main` shrinks to exactly that:
//! parse argv into a [`cli::Command`], map it to the [`Screen`] that command opens on, build
//! the machine's [`RunContext`], and hand an [`App`] to [`runtime::run`]. No backup/restore
//! logic lives here.
//!
//! This is the Foundation build (#63): the runtime, the pure `update`/render split, and the
//! `TestBackend` harness are in place, but the screens themselves are placeholders. The
//! inquire-based command modules below stay compiled — `inquire`/`indicatif` are not removed
//! until later builds port each screen to ratatui (spec #62's build sequencing) — but `main`
//! no longer dispatches to them, so they are `allow(dead_code)` until each is retired.
//!
//! `main` is one of the two places (tests being the other) AGENTS.md's coding conventions
//! exempt from the no-`unwrap`/`expect`/`panic!` rule — kept narrow here: the only thing this
//! function does with a `Result` is print it and pick an exit code, never `unwrap`/`expect`.

// `#[cfg(test)]` modules in this crate compile under `--cfg test`, so this allow only ever
// applies to them — see core/src/lib.rs for the same pattern.
#![cfg_attr(test, allow(clippy::disallowed_methods, clippy::disallowed_macros))]

mod app;
mod cli;
mod encryption_screen;
mod reflow;
mod restore_screen;
mod run_screen;
mod runtime;
mod system;
mod what_screen;
mod where_step;
mod wizard;

// Retired screen by screen per spec #62: these inquire-based modules stay compiled (so
// `inquire`/`indicatif` and their existing tests survive the interim) but are no longer
// reachable from `main` now that it drives the ratatui runtime. Each is deleted as its
// ratatui replacement lands.
#[allow(dead_code)]
mod backup_cmd;
#[allow(dead_code)]
mod encryption_cmd;
#[allow(dead_code)]
mod restore_cmd;
#[allow(dead_code)]
mod select_cmd;
#[allow(dead_code)]
mod where_screen;

use std::process::ExitCode;

use app::{App, Screen};

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
    let command = cli::parse_args(args).map_err(anyhow::Error::msg)?;
    let run_context = system::build_run_context()?;

    // `restore` carries its Backup-dir argument into a dedicated constructor (the flow probes
    // it to pick its opening screen, #72); the argument-less routes just seed their `Screen`.
    let app = match command {
        cli::Command::Restore(args) => App::new_restore(args, run_context),
        cli::Command::Backup => App::new(Screen::Backup, run_context),
        cli::Command::Select => App::new(Screen::Select, run_context),
        cli::Command::Where => App::new(Screen::Where, run_context),
    };
    runtime::run(app)
}
