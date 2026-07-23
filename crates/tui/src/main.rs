//! `linux-backup`'s TUI binary entry point.
//!
//! Per ADR-0005 / spec #62, the interactive surface is one ratatui `App` reducer driven by a
//! single shared runtime seeded with an initial [`Screen`]. `main` shrinks to exactly that:
//! parse argv into a [`cli::Command`], map it to the [`Screen`] that command opens on, build
//! the machine's [`RunContext`], and hand an [`App`] to [`runtime::run`]. No backup/restore
//! logic lives here.
//!
//! Every screen is now on the ratatui app frame (spec #62's build sequencing complete), so the
//! interim `inquire`-based command modules and the `inquire`/`indicatif` dependencies they held
//! alive have been retired from the workspace (#73).
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
mod glyphs;
mod help;
mod keymap;
mod path_autocomplete;
mod pending_probe;
mod reflow;
mod restore_screen;
mod run_screen;
mod runtime;
mod system;
mod what_screen;
mod where_step;
mod wizard;

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
    // The no-emoji fallback is an ambient run fact decided once here at the `tui` boundary from
    // the process locale (ADR-0004: never read inside `core`), then threaded through render.
    let glyphs = glyphs::Glyphs::detect(|name| std::env::var(name).ok());

    // `restore` carries its Backup-dir argument into a dedicated constructor (the flow probes
    // it to pick its opening screen, #72); the argument-less routes just seed their `Screen`.
    let app = match command {
        cli::Command::Restore(args) => App::new_restore(args, run_context, glyphs),
        cli::Command::Backup => App::new(Screen::Backup, run_context, glyphs),
        cli::Command::Select => App::new(Screen::Select, run_context, glyphs),
        cli::Command::Where => App::new(Screen::Where, run_context, glyphs),
    };
    runtime::run(app)
}
