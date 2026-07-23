//! Managed Tauri state: `core::SelectionModel` stays the single source of truth (ADR-0008
//! Decision 2), owned here exactly as the ratatui `App` owns it (ADR-0005 Decision 1). The
//! React frontend holds only derived/view state and reflects this via `TanStack` Query.

use std::sync::{Arc, Mutex};

use linux_backup_core::scan::CategoryScan;
use linux_backup_core::{CancelToken, RunContext, SelectionModel};

use crate::encryption::EncryptionDecision;

/// The `tauri::State` wrapper around the authoritative `SelectionModel`. A `Mutex` because
/// Tauri may run command handlers on different threads across invocations, and the command
/// layer here is a thin, synchronous mirror (ADR-0008: "no selection logic" in this layer)
/// with no need for anything heavier than a lock per call.
pub struct SelectionState(pub Mutex<SelectionModel>);

impl SelectionState {
    pub fn new(selection: SelectionModel) -> Self {
        Self(Mutex::new(selection))
    }
}

/// The `tauri::State` wrapper around the immutable [`RunContext`] the GUI was launched
/// with. `add_custom_path` needs `home`/`privilege` for `catalog::validate_custom_path`. No
/// `Mutex`: nothing in this crate mutates a `RunContext` after a run starts (ADR-0004), so
/// this is a plain read-only share across command invocations.
pub struct RunContextState(pub RunContext);

/// The one-time `scan::scan_taxonomy` result, computed once at startup (`crate::run`) and
/// reused for every `["tree", prefix]` root read and `select_category` call thereafter,
/// rather than re-walking the filesystem on every one of those. This matters because *all
/// seven* mutation commands invalidate the `["tree", ...]` query family, so without this
/// cache a single Entry-level checkbox click (which can never change which Categories exist
/// or what's Hidden) would still force a full allowlist walk on the critical path of the
/// root query's resulting refetch — expensive on a real ~40,000-file KDE Plasma icon theme
/// (ADR-0006's dated addendum). The on-disk set backing Hide-empty is session-invariant
/// (it depends on the filesystem, never on the `SelectionModel`'s active/withheld split, which
/// `SelectionState` already serves fresh on every read) — `tui`'s own `seed_wizard` makes
/// the identical one-scan-per-session assumption, scanning once at wizard start and never
/// again. No `Mutex`: this is computed once and never mutated afterwards.
pub struct ScanCacheState(pub Vec<CategoryScan>);

/// The `tauri::State` wrapper around the gui-side [`EncryptionDecision`] (issue #101,
/// ADR-0007 D3/D5's "recorded context"): which passphrase (if any) is committed, and — while
/// plaintext — the Sensitive set that decision was made against. A `Mutex` for the same reason
/// as [`SelectionState`]: command handlers may run on different threads across invocations.
pub struct EncryptionState(pub Mutex<EncryptionDecision>);

impl EncryptionState {
    pub fn new(decision: EncryptionDecision) -> Self {
        Self(Mutex::new(decision))
    }
}

/// The in-flight backup run's [`CancelToken`] (issue #102), `None` between runs. A `Mutex`
/// rather than a bare `CancelToken` because a run doesn't exist until `run_backup` starts one
/// — the separate `cancel_backup` command (ADR-0008 Decision 3: front-half-only cancellation
/// lives on its own command, keeping `Progress` strictly one-way) needs somewhere to find the
/// *current* run's token without the two commands sharing anything but this slot. Only ever
/// holds one run's token at a time: the ADR-0008 run model is one `useMutation` per run, so a
/// second `run_backup` cannot start while this is `Some`.
///
/// Wrapped in an `Arc` (unlike [`SelectionState`]/[`EncryptionState`]) so `run_backup` — an
/// *async* command — can pull an owned, `'static` handle out of the `tauri::State<'_, _>` it's
/// given (via `.inner().clone()`) before its `spawn_blocking().await`, then use that same handle
/// again afterward to clear the slot. A bare `State<'_, T>` borrows the invoke call's lifetime,
/// which cannot survive being held across an `.await` in a future Tauri requires to be
/// `'static`; the owned `Arc` clone sidesteps that entirely.
#[derive(Default, Clone)]
pub struct RunCancelState(pub Arc<Mutex<Option<CancelToken>>>);

/// The blocked restore worker's pending collision-confirm reply slot (issue #103): `Some`
/// only for the window between `core::restore`'s `CollisionPolicy::Confirm` callback sending
/// its one-shot `CollisionRequest` and the reply arriving. `run_restore`'s worker thread
/// stashes the `std::sync::mpsc::Sender` half here immediately before it blocks on the
/// matching `Receiver`; `resolve_collision` looks here for where to deliver the user's single
/// yes/no. `Arc`-wrapped for the identical `'static`-across-`.await` reason as
/// [`RunCancelState`] — `run_restore` is itself an async command holding this across its own
/// `spawn_blocking().await`.
#[derive(Default, Clone)]
pub struct CollisionReplyState(pub Arc<Mutex<Option<std::sync::mpsc::Sender<bool>>>>);
