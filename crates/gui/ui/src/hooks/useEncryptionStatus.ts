import { useQuery } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import type { EncryptionStatusDto } from '@/types/encryption-status'

/**
 * The lazy `["encryption-status"]` query key (issue #101, ADR-0008 D2 "Views out"). Exported
 * so a Selection mutation's `onSuccess` invalidates this exact key — a WHAT edit can change
 * Auto-insist and the encryption step's completeness/invalidation (ADR-0007 D5), so it belongs
 * alongside `treeRootQueryKey`/`selectionSummaryQueryKey` in the invalidate-don't-return set.
 */
export const encryptionStatusQueryKey = ['encryption-status'] as const

/**
 * Reflects the encryption step's derived state: Auto-insist (is anything Sensitive) plus
 * whether a decision exists and is still valid against the live Selection (ADR-0007 D2/D4/D5).
 * Holds no client-only state of its own — `core::crypto::evaluate` plus the gui-side
 * `EncryptionDecision` are the sole sources of truth this reflects.
 */
export function useEncryptionStatus() {
  return useQuery({
    queryKey: encryptionStatusQueryKey,
    queryFn: () => invoke<EncryptionStatusDto>('encryption_status'),
  })
}
