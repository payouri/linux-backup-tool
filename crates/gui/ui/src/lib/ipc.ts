import { invoke as tauriInvoke } from '@tauri-apps/api/core'

/**
 * The one seam between this frontend and Tauri's IPC layer. Every hook calls through this
 * module rather than importing `@tauri-apps/api/core` directly, so the frontend test seam
 * (spec #97's Testing Decisions: "the Tauri IPC boundary MOCKED — a fake invoke/Channel")
 * has exactly one module to mock (`vi.mock('@/lib/ipc')`) instead of reaching into Tauri's
 * own package from every test file.
 */
export const invoke = tauriInvoke
