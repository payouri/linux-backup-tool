import { type QueryClient, useMutation, useQueryClient } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import { encryptionStatusQueryKey } from '@/hooks/useEncryptionStatus'
import { selectionSummaryQueryKey } from '@/hooks/useSelectionSummary'
import { treeRootQueryKey } from '@/hooks/useTree'
import type { SerializableCoreError } from '@/types/errors'

/**
 * The thin 1:1 mirror of `SelectionModel`'s seven mutation methods (ADR-0008 D2, issue
 * #100): `select_category`, `deselect_category`, `add_custom_path`, `deselect_entry`,
 * `reselect_entry`, `deselect_root`, `reselect_root`. Every one of these `useMutation`
 * hooks shares the same **invalidate, don't return** convention — the command's own return
 * carries no derived state, so `onSuccess` invalidates every `["tree", ...]` query,
 * `["selection-summary"]`, and `["encryption-status"]` (issue #101: a WHAT edit can change
 * Auto-insist and invalidate a completed plaintext decision, ADR-0007 D5) and lets TanStack
 * Query refetch whatever is actually mounted.
 */
function invalidateSelectionViews(queryClient: QueryClient) {
  queryClient.invalidateQueries({ queryKey: treeRootQueryKey })
  queryClient.invalidateQueries({ queryKey: selectionSummaryQueryKey })
  queryClient.invalidateQueries({ queryKey: encryptionStatusQueryKey })
}

/** Category-level toggle ON (story 7): mirrors `SelectionModel::select_category`. */
export function useSelectCategory() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (category) => invoke<void>('select_category', { category }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/** Category-level toggle OFF (story 7): mirrors `SelectionModel::deselect_category`. */
export function useDeselectCategory() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (category) => invoke<void>('deselect_category', { category }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/**
 * Adds a Custom path (stories 14/15/16): mirrors `SelectionModel::add_custom_path`, but
 * genuinely fallible — `catalog::validate_custom_path` runs first on the Rust side, so a
 * path outside the privilege-aware scope rejects with a typed `SerializableCoreError`
 * (`mutation.error`) rather than throwing a generic `Error` or crashing the UI.
 */
export function useAddCustomPath() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (absolutePath) => invoke<void>('add_custom_path', { absolutePath }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/** Deselects (Drill-down) a single Entry — mirrors `SelectionModel::deselect_entry`. */
export function useDeselectEntry() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (relativePath) => invoke<void>('deselect_entry', { relativePath }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/** Reselects a single previously-deselected Entry — mirrors
 * `SelectionModel::reselect_entry`. */
export function useReselectEntry() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (relativePath) => invoke<void>('reselect_entry', { relativePath }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/** Deselects a whole Subtree (story 11) — mirrors `SelectionModel::deselect_root`. */
export function useDeselectRoot() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (root) => invoke<void>('deselect_root', { root }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}

/** Reverses a Subtree deselection (stories 12/13, prefix-wins) — mirrors
 * `SelectionModel::reselect_root`. */
export function useReselectRoot() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (root) => invoke<void>('reselect_root', { root }),
    onSuccess: () => invalidateSelectionViews(queryClient),
  })
}
