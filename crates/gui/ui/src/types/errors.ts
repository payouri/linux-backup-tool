/**
 * Mirrors `crates/gui/src/error.rs`'s `SerializableCoreError` — the single lossy-flattened
 * error shape (ADR-0008 Decisions 2/3) every fallible command rejects with. Tauri's
 * `invoke()` rejects a failed `#[tauri::command]` with the `Err` variant's serialized
 * value directly, so a rejected mutation's `error` is this shape, not a generic `Error`.
 */
export interface SerializableCoreError {
  kind: string
  message: string
}
