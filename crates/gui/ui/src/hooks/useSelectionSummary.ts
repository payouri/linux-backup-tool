import { useQuery } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import type { SelectionSummaryDto } from '@/types/selection-summary'

/**
 * The lazy `["selection-summary"]` query key (ADR-0008 Decision 2 "Views out"). Exported so
 * a future mutation hook invalidates this exact key (the "invalidate, don't return"
 * convention) rather than a hand-typed string literal drifting from this one.
 */
export const selectionSummaryQueryKey = ['selection-summary'] as const

/**
 * Reflects the managed `core::SelectionModel`'s summary — entry count, total size,
 * emptiness, whether Auto-insist requires encryption, and any warnings — via TanStack
 * Query. Holds no client-only state of its own (ADR-0008: `core` stays authoritative; the
 * frontend reflects it).
 */
export function useSelectionSummary() {
  return useQuery({
    queryKey: selectionSummaryQueryKey,
    queryFn: () => invoke<SelectionSummaryDto>('selection_summary'),
  })
}
