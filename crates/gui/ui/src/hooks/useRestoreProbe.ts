import { useQuery } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import type { RestoreProbeDto } from '@/types/restore'

/** The `["restore-probe", backupDir]` query key (issue #103). */
export function restoreProbeQueryKey(backupDir: string) {
  return ['restore-probe', backupDir] as const
}

/**
 * The Restore flow's entry probe (stories 39/40): whether the picked Backup dir exists and,
 * only if it does, whether it's encrypted — so the passphrase field is shown only when the
 * Backup actually needs one. Disabled while no Backup dir has been picked yet.
 */
export function useRestoreProbe(backupDir: string | null) {
  return useQuery({
    queryKey: restoreProbeQueryKey(backupDir ?? ''),
    queryFn: () => invoke<RestoreProbeDto>('restore_probe', { backupDir }),
    enabled: backupDir !== null && backupDir.length > 0,
    // A directory's existence/encryption shape right now is a filesystem fact, not something
    // to retry away.
    retry: false,
  })
}
