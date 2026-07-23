//! Tauri commands: the thin 1:1 mirror ADR-0008 Decision 2 describes. This module carries
//! no selection logic of its own — it locks the managed state, delegates to `core`, and
//! (for a mutation) leaves invalidation to the frontend rather than returning derived state.
//!
//! Issue #99's walking skeleton wired the lazy `["selection-summary"]` read. Issue #100
//! wires the lazy `["tree", prefix]` read and the seven `SelectionModel` mutation mirrors
//! (`select_category`, `deselect_category`, `add_custom_path`, `deselect_entry`,
//! `reselect_entry`, `deselect_root`, `reselect_root`), each returning
//! `Result<(), SerializableCoreError>` — uniform across all seven even though only
//! `add_custom_path` is genuinely fallible today (ADR-0008 Decision 2). `clippy::
//! unnecessary_wraps` fires on the other six (their body can never actually return `Err`
//! yet); allowed module-wide rather than repeated per function, since every mutation
//! command shares the identical justification: the frontend's `useMutation` call sites
//! match one uniform `Result` shape across all seven, not six-plus-one.
//!
//! Issue #101 adds the encryption step's `["encryption-status"]` read, its two decision
//! mutations (`set_passphrase`, `choose_plaintext`), and the WHERE step's `home_directory`
//! read and `where_check` pre-flight — all still no more than a lock-and-delegate over
//! `core`/the gui-side `EncryptionDecision` (ADR-0007's "recorded context", `crate::encryption`).
#![allow(clippy::unnecessary_wraps)]

use std::path::{Path, PathBuf};

use secrecy::SecretString;
use tauri::State;

use linux_backup_core::{catalog, location, CoreError, SelectionModel};

use crate::dto::{self, EncryptionStatusDto, SelectionSummaryDto, WhereCheckDto};
use crate::error::SerializableCoreError;
use crate::state::{EncryptionState, RunContextState, ScanCacheState, SelectionState};
use crate::tree::{self, TreeNodeDto};

/// Recovers a poisoned lock rather than propagating it: a prior command handler panicking
/// mid-mutation shouldn't crash every future command over one bad handler, and every lock
/// this module takes is held only across infallible, panic-free `core` calls anyway.
fn lock_selection(state: &SelectionState) -> std::sync::MutexGuard<'_, SelectionModel> {
    state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Recovers a poisoned lock the same way [`lock_selection`] does — see that function's doc.
fn lock_encryption(
    state: &EncryptionState,
) -> std::sync::MutexGuard<'_, crate::encryption::EncryptionDecision> {
    state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Builds the `["selection-summary"]` query response from the managed `SelectionModel`.
/// Infallible: every field it reads (`entries`, `total_size`, `is_empty`,
/// `encryption_required`, `warnings`) is a plain, non-failing accessor, so this command
/// returns the DTO directly rather than a `Result<_, SerializableCoreError>` — unlike the
/// mutation commands below, which are genuinely fallible and keep a uniform `Result` shape.
// `State<'_, T>` is a thin `&T` wrapper (see `tauri::State`'s definition) that `#[tauri::
// command]`'s `CommandArg` extraction requires by value — there is no by-reference form to
// take instead, so `needless_pass_by_value` is a false positive here.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn selection_summary(state: State<'_, SelectionState>) -> SelectionSummaryDto {
    let selection = lock_selection(&state);
    SelectionSummaryDto::from(&*selection)
}

/// The lazy `["tree", prefix]` query (issue #100): `category: None` returns the root level
/// (one node per Category surviving Hide-empty, plus Custom paths unconditionally);
/// `category: Some(name)` returns `name`'s own directory/file children under `path`
/// (`None`/empty meaning the Category's own top level).
///
/// The root level reads the cached [`ScanCacheState`] rather than re-scanning the
/// filesystem — see that type's doc for why a fresh `scan::scan_taxonomy` call here would
/// put a full allowlist walk on every mutation's `['tree']`-invalidation-triggered refetch,
/// not just an actual Category-membership change. Every deeper level is pure presentation
/// grouping over the already-scanned `SelectionModel` (ADR-0006 D5), no filesystem read at
/// all.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn tree(
    selection_state: State<'_, SelectionState>,
    scan_cache_state: State<'_, ScanCacheState>,
    category: Option<String>,
    path: Option<String>,
) -> Vec<TreeNodeDto> {
    let selection = lock_selection(&selection_state);
    match category {
        None => tree::category_nodes(&selection, &scan_cache_state.0),
        Some(name) => {
            let prefix = path.map(PathBuf::from).unwrap_or_default();
            tree::category_children(&selection, &name, &prefix)
        }
    }
}

/// Category-level toggle ON (story 7): looks `category` up in the cached
/// [`ScanCacheState`] (a Category the Selection has never touched carries none of its
/// Entries yet, so its full on-disk set has to come from the scan, not the model) and
/// activates every one of its Entries via `SelectionModel::select_category`. A no-op if
/// `category` names no known Category (nothing scanned matches) or is Hidden with zero
/// on-disk Entries. See [`ScanCacheState`]'s doc for why this reads the cache rather than
/// re-scanning on every call.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn select_category(
    selection_state: State<'_, SelectionState>,
    scan_cache_state: State<'_, ScanCacheState>,
    category: String,
) -> Result<(), SerializableCoreError> {
    if let Some(scan) = scan_cache_state
        .0
        .iter()
        .find(|scan| scan.category.name == category)
    {
        lock_selection(&selection_state).select_category(scan);
    }
    Ok(())
}

/// Category-level toggle OFF (story 7): a thin mirror of
/// `SelectionModel::deselect_category`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn deselect_category(
    state: State<'_, SelectionState>,
    category: String,
) -> Result<(), SerializableCoreError> {
    lock_selection(&state).deselect_category(&category);
    Ok(())
}

/// Adds a Custom path (stories 14/15/16): validates `absolute_path` against the
/// privilege-aware scope rule (`catalog::validate_custom_path` — any absolute path when
/// privileged, else under `$HOME`) before touching the Selection, surfacing a rejection as
/// a typed [`SerializableCoreError`] rather than a crash.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn add_custom_path(
    selection_state: State<'_, SelectionState>,
    run_context_state: State<'_, RunContextState>,
    absolute_path: String,
) -> Result<(), SerializableCoreError> {
    let ctx = &run_context_state.0;
    let path = PathBuf::from(&absolute_path);
    catalog::validate_custom_path(&path, &ctx.home, ctx.privilege)
        .map_err(|error| SerializableCoreError::from(&error))?;

    lock_selection(&selection_state).add_custom_path(&path, &ctx.home);
    Ok(())
}

/// Deselects (Drill-down) a single Entry — a thin mirror of
/// `SelectionModel::deselect_entry`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn deselect_entry(
    state: State<'_, SelectionState>,
    relative_path: String,
) -> Result<(), SerializableCoreError> {
    lock_selection(&state).deselect_entry(Path::new(&relative_path));
    Ok(())
}

/// Reselects a single previously-deselected Entry — a thin mirror of
/// `SelectionModel::reselect_entry`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn reselect_entry(
    state: State<'_, SelectionState>,
    relative_path: String,
) -> Result<(), SerializableCoreError> {
    lock_selection(&state).reselect_entry(Path::new(&relative_path));
    Ok(())
}

/// Deselects a whole Subtree (story 11) — a thin mirror of
/// `SelectionModel::deselect_root`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn deselect_root(
    state: State<'_, SelectionState>,
    root: String,
) -> Result<(), SerializableCoreError> {
    lock_selection(&state).deselect_root(Path::new(&root));
    Ok(())
}

/// Reverses a Subtree deselection (story 12, prefix-wins re-inclusion, story 13) — a thin
/// mirror of `SelectionModel::reselect_root`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn reselect_root(
    state: State<'_, SelectionState>,
    root: String,
) -> Result<(), SerializableCoreError> {
    lock_selection(&state).reselect_root(Path::new(&root));
    Ok(())
}

/// The lazy `["encryption-status"]` query (issue #101, ADR-0007 D2/D4/D5): the encryption
/// step's derived state — Auto-insist plus the gui-side [`EncryptionDecision`]'s
/// completeness/invalidation — reflecting the live Selection each call, so a WHAT edit that
/// changes sensitivity is picked up the moment this is invalidated/refetched, never a stale
/// snapshot. Infallible, like `selection_summary`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn encryption_status(
    selection_state: State<'_, SelectionState>,
    encryption_state: State<'_, EncryptionState>,
) -> EncryptionStatusDto {
    let selection = lock_selection(&selection_state);
    let decision = lock_encryption(&encryption_state);
    dto::encryption_status(&selection, &decision)
}

/// Commits a passphrase (stories 22/23) — the encryption safe harbor (ADR-0007 D5). A blank
/// passphrase is rejected as [`CoreError::BlankPassphrase`] (story 24) before it ever touches
/// the gui-side decision, mirroring `archive::encrypt`'s own hard reject at the point a
/// passphrase is actually used — this is a preflight of that same invariant, not a new one.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn set_passphrase(
    encryption_state: State<'_, EncryptionState>,
    passphrase: String,
) -> Result<(), SerializableCoreError> {
    if passphrase.is_empty() {
        return Err(SerializableCoreError::from(&CoreError::BlankPassphrase));
    }
    lock_encryption(&encryption_state).set_passphrase(SecretString::from(passphrase));
    Ok(())
}

/// Records a deliberate plaintext decision (stories 22/25): the non-Sensitive "leave
/// plaintext" path, and — for a Sensitive Selection — overriding Auto-insist. The frontend is
/// responsible for only calling this after its own second confirmation naming the Categories
/// about to land in plaintext (`encryption_status`'s `sensitiveCategories`); this command
/// itself carries no confirmation gate of its own; see this module's doc for why the command
/// layer holds no such policy.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn choose_plaintext(
    selection_state: State<'_, SelectionState>,
    encryption_state: State<'_, EncryptionState>,
) -> Result<(), SerializableCoreError> {
    let selection = lock_selection(&selection_state);
    lock_encryption(&encryption_state).choose_plaintext(&selection);
    Ok(())
}

/// The real machine's `$HOME`, so the WHERE step (story 26) can quick-offer it as an Output
/// location without the frontend re-deriving it.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn home_directory(run_context_state: State<'_, RunContextState>) -> String {
    run_context_state.0.home.to_string_lossy().into_owned()
}

/// The WHERE step's pre-flight check (issue #101, stories 26/27): validates `location` is a
/// writable directory and pre-checks the Backup-dir collision against the *same* name
/// `backup()` will later compute (`backup_dir_name`), so a collision is learned here, not
/// mid-run. Delegates entirely to `location::resolve_selection` rather than re-implementing
/// the writable/collision checks, so there is one implementation of both refusals instead of
/// two that could drift apart; `available_space` is `None` (no live disk enumeration in this
/// slice — story 25/32's free-space warning is a candidate for a later build), so the
/// advisory free-space warning never fires here, only the two hard refusals do.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn where_check(
    run_context_state: State<'_, RunContextState>,
    selection_state: State<'_, SelectionState>,
    location: String,
) -> Result<WhereCheckDto, SerializableCoreError> {
    let ctx = &run_context_state.0;
    let selection = lock_selection(&selection_state);
    let name = linux_backup_core::backup_dir_name(ctx);
    location::resolve_selection(Path::new(&location), &name, selection.total_size(), None)
        .map(|resolved| WhereCheckDto::from(&resolved))
        .map_err(|error| SerializableCoreError::from(&error))
}
