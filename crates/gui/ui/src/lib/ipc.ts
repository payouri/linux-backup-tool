import { invoke as tauriInvoke } from '@tauri-apps/api/core'

/**
 * The one seam between this frontend and Tauri's IPC layer. Every hook calls through this
 * module rather than importing `@tauri-apps/api/core` directly, so the frontend test seam
 * (spec #97's Testing Decisions: "the Tauri IPC boundary MOCKED — a fake invoke/Channel")
 * has exactly one module to mock (`vi.mock('@/lib/ipc')`) instead of reaching into Tauri's
 * own package from every test file.
 */
export const invoke = tauriInvoke

/**
 * Re-exported alongside `invoke` (issue #102) so `useRun`'s progress channel construction goes
 * through this same one seam — a test mocking `@/lib/ipc` supplies a fake `Channel` class
 * instead of a real IPC-backed one, exactly as it supplies a fake `invoke`. Re-exported via
 * `export {…} from` (not a `const` alias) so the name stays usable both as the value
 * (`new Channel()`) and as the generic type (`Channel<ProgressEvent>`), matching the real
 * `@tauri-apps/api/core` export's own dual nature.
 */
export { Channel } from '@tauri-apps/api/core'
