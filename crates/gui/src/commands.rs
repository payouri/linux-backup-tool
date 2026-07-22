//! Tauri commands: the thin 1:1 mirror ADR-0008 Decision 2 describes. This module carries
//! no selection logic of its own — it locks the managed state, delegates to `core`, and
//! (for a future mutation) would invalidate query keys rather than return derived state.
//! Issue #99's walking skeleton wires exactly one command: the lazy `["selection-summary"]`
//! read.

use tauri::State;

use crate::dto::SelectionSummaryDto;
use crate::state::SelectionState;

/// Builds the `["selection-summary"]` query response from the managed `SelectionModel`.
/// Infallible: every field it reads (`entries`, `total_size`, `is_empty`,
/// `encryption_required`, `warnings`) is a plain, non-failing accessor, so this command
/// returns the DTO directly rather than a `Result<_, SerializableCoreError>` — unlike the
/// mutation commands ADR-0008 describes (`add_custom_path`, ...), which are genuinely
/// fallible and are this crate's next build.
///
/// A poisoned lock (a prior command handler panicking mid-mutation) is recovered rather than
/// propagated: a stale-but-readable summary beats crashing every future read over one bad
/// handler, and today's handlers hold the lock only across infallible `core` calls anyway.
// `State<'_, T>` is a thin `&T` wrapper (see `tauri::State`'s definition) that `#[tauri::
// command]`'s `CommandArg` extraction requires by value — there is no by-reference form to
// take instead, so `needless_pass_by_value` is a false positive here.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn selection_summary(state: State<'_, SelectionState>) -> SelectionSummaryDto {
    let selection = state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    SelectionSummaryDto::from(&*selection)
}
