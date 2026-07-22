//! `linux-backup`'s Tauri GUI backend (ADR-0008, spec #97): a Rust backend depending on
//! `core` only, exposing `core::SelectionModel` to the React frontend (`crates/gui/ui/`)
//! over a JSON IPC boundary. Issue #99 is the walking skeleton: one managed
//! `SelectionModel`, seeded from Smart defaults exactly as `tui`'s `seed_wizard` does, and
//! one lazy `["selection-summary"]` query command.
//!
//! `#[cfg(test)]` modules in this crate compile under `--cfg test`, so this allow only ever
//! applies to them — see `core/src/lib.rs` for the same pattern.
#![cfg_attr(test, allow(clippy::disallowed_methods, clippy::disallowed_macros))]

mod commands;
mod dto;
mod error;
mod run_context;
mod state;
mod tree;

pub use dto::{DirectoryStateDto, SelectionSummaryDto};
pub use error::SerializableCoreError;
pub use tree::{category_children, category_nodes, TreeNodeDto, TreeNodeKindDto};

use linux_backup_core::scan::CategoryScan;
use linux_backup_core::{scan, SelectionModel};

use state::{RunContextState, ScanCacheState, SelectionState};

/// Builds the initial `SelectionModel` a fresh GUI window opens on: `scanned` filtered to
/// each Category's Smart default — the exact derivation `tui`'s `seed_wizard`
/// (`crates/tui/src/app.rs`) performs for the ratatui wizard, so both surfaces open on the
/// same Selection for identical inputs (spec #97, story 4). Takes the already-scanned
/// taxonomy rather than a `RunContext` so `run` below scans the filesystem exactly once and
/// shares that one result with both the seeded Selection and the cached
/// [`state::ScanCacheState`] the `tree`/`select_category` commands read from thereafter.
fn seed_selection(scanned: &[CategoryScan]) -> SelectionModel {
    SelectionModel::from_smart_defaults(scanned)
}

/// Builds and runs the Tauri application. Kept out of `main.rs` so the walking skeleton's
/// wiring (state seeding, command registration) is reachable from `crates/gui/tests/` via
/// this crate's `pub` surface, not just from the `linux-backup-gui` binary.
///
/// # Errors
///
/// Returns an error if the real machine's `$HOME` cannot be resolved (see
/// [`run_context::build_run_context`]) or if the Tauri runtime itself fails to start.
pub fn run() -> anyhow::Result<()> {
    let ctx = run_context::build_run_context()?;
    let scanned = scan::scan_taxonomy(&ctx);
    let selection = seed_selection(&scanned);

    // `tauri::generate_context!()` expands to `tauri`-crate-owned codegen that unwraps while
    // parsing the `tauri.conf.json` this crate's `build.rs` (`tauri-build`) already
    // validated at compile time — the panics live in a dependency's macro output, not in
    // logic this crate could instead express as a `Result`, so the workspace's no-
    // unwrap/expect lint is waived narrowly for this one macro invocation.
    #[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
    let context = tauri::generate_context!();

    tauri::Builder::default()
        .manage(SelectionState::new(selection))
        .manage(RunContextState(ctx))
        .manage(ScanCacheState(scanned))
        .invoke_handler(tauri::generate_handler![
            commands::selection_summary,
            commands::tree,
            commands::select_category,
            commands::deselect_category,
            commands::add_custom_path,
            commands::deselect_entry,
            commands::reselect_entry,
            commands::deselect_root,
            commands::reselect_root,
        ])
        .run(context)
        .map_err(|err| anyhow::anyhow!("error while running the Tauri application: {err}"))
}
