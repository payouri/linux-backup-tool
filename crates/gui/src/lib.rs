//! `linux-backup`'s Tauri GUI backend (ADR-0008, spec #97): a Rust backend depending on
//! `core` only, exposing `core::SelectionModel` to the React frontend (`crates/gui/ui/`)
//! over a JSON IPC boundary. Issue #99 is the walking skeleton: one managed
//! `SelectionModel`, seeded from Smart defaults exactly as `tui`'s `seed_wizard` does, and
//! one lazy `["selection-summary"]` query command. Issue #100 adds the lazy `["tree",
//! prefix]` explorer and the seven Selection mutations. Issue #101 adds the encryption step
//! (a managed gui-side `EncryptionDecision`, ADR-0007's ported "recorded context") and the
//! WHERE step's collision pre-check. Issue #102 adds the RUN model: an async `run_backup`
//! command driving `core::backup` on a `spawn_blocking` worker, progress over a per-invocation
//! `tauri::ipc::Channel` (`run.rs`), and the front-half-only `cancel_backup` command.
//!
//! `#[cfg(test)]` modules in this crate compile under `--cfg test`, so this allow only ever
//! applies to them — see `core/src/lib.rs` for the same pattern.
#![cfg_attr(test, allow(clippy::disallowed_methods, clippy::disallowed_macros))]

mod commands;
mod dto;
mod encryption;
mod error;
mod restore;
mod run;
mod run_context;
mod state;
mod tree;

pub use dto::{
    encryption_status, DirectoryStateDto, EncryptionStatusDto, SelectionSummaryDto, WhereCheckDto,
};
pub use encryption::EncryptionDecision;
pub use error::SerializableCoreError;
pub use restore::{
    OwnershipOutcomeDto, RestoreEventDto, RestoreFailureDto, RestoreOutcomeDto, RestoreProbeDto,
    RestoreRunOutcomeDto, SkippedCollisionDto,
};
pub use run::{BackupOutcomeDto, ManifestSummaryDto, PhaseDto, ProgressEventDto, RunOutcomeDto};
pub use tree::{category_children, category_nodes, TreeNodeDto, TreeNodeKindDto};

use linux_backup_core::scan::CategoryScan;
use linux_backup_core::{scan, SelectionModel};

use state::{
    CollisionReplyState, EncryptionState, RunCancelState, RunContextState, ScanCacheState,
    SelectionState,
};

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
    // Installed once, before anything can panic (issue #102, ADR-0008 Decision 3): keeps a
    // `spawn_blocking` worker panic diagnosable host-side even though its `Internal` outcome
    // never leaks the raw message to the webview. Mirrors `crates/tui`'s single-install panic
    // hook (ADR-0005 Decision 7).
    run::install_panic_hook();

    let ctx = run_context::build_run_context()?;
    let scanned = scan::scan_taxonomy(&ctx);
    let selection = seed_selection(&scanned);
    let encryption_decision = EncryptionDecision::new(&selection);

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
        .manage(EncryptionState::new(encryption_decision))
        .manage(RunCancelState::default())
        .manage(CollisionReplyState::default())
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
            commands::encryption_status,
            commands::set_passphrase,
            commands::choose_plaintext,
            commands::home_directory,
            commands::where_check,
            run::run_backup_impl::run_backup,
            run::cancel_backup,
            restore::restore_probe,
            restore::run_restore_impl::run_restore,
            restore::cancel_restore,
            restore::resolve_collision,
        ])
        .run(context)
        .map_err(|err| anyhow::anyhow!("error while running the Tauri application: {err}"))
}
