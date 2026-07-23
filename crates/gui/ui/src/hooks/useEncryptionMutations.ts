import { useMutation, useQueryClient } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import { encryptionStatusQueryKey } from '@/hooks/useEncryptionStatus'
import type { SerializableCoreError } from '@/types/errors'

/**
 * Commits a passphrase (stories 22/23) — the encryption safe harbor (ADR-0007 D5). A blank
 * passphrase rejects with `SerializableCoreError`'s `kind: "blank_passphrase"` (story 24),
 * surfaced as `mutation.error` rather than a thrown crash — the same convention
 * `useAddCustomPath` already established for a fallible mutation.
 */
export function useSetPassphrase() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, string>({
    mutationFn: (passphrase) => invoke<void>('set_passphrase', { passphrase }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: encryptionStatusQueryKey }),
  })
}

/**
 * Records a deliberate plaintext decision (stories 22/25): the non-Sensitive "leave plaintext"
 * path, and — for a Sensitive Selection — overriding Auto-insist. The caller is responsible
 * for only invoking this after its own second confirmation naming the Categories about to land
 * in plaintext (`useEncryptionStatus`'s `sensitiveCategories`) — this hook carries no
 * confirmation gate of its own, mirroring the Rust command it mirrors.
 */
export function useChoosePlaintext() {
  const queryClient = useQueryClient()
  return useMutation<void, SerializableCoreError, void>({
    mutationFn: () => invoke<void>('choose_plaintext'),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: encryptionStatusQueryKey }),
  })
}
