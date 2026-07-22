//! Managed Tauri state: `core::SelectionModel` stays the single source of truth (ADR-0008
//! Decision 2), owned here exactly as the ratatui `App` owns it (ADR-0005 Decision 1). The
//! React frontend holds only derived/view state and reflects this via `TanStack` Query.

use std::sync::Mutex;

use linux_backup_core::SelectionModel;

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
