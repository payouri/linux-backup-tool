//! The `SelectionModel`: Category defaults, precedence resolution, Drill-down toggles, and
//! derived state (`encryption_required`, `total_size`, `selected_paths`, `warnings`).
//!
//! This is `core`'s authoritative model of what will be backed up; `crates/tui` is a thin
//! view over it, owning only cursor/scroll state and rendering.
