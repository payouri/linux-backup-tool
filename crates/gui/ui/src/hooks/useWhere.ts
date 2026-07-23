import { useQuery } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import type { WhereCheckDto } from '@/types/where-check'

/** The real machine's `$HOME` (story 26), so the WHERE step can quick-offer it without the
 * frontend re-deriving it from anywhere else. */
export function useHomeDirectory() {
  return useQuery({
    queryKey: ['home-directory'] as const,
    queryFn: () => invoke<string>('home_directory'),
    // `$HOME` never changes for the life of the window — no reason to ever refetch it.
    staleTime: Infinity,
  })
}

/** The `["where-check", location]` query key (issue #101). */
export function whereCheckQueryKey(location: string) {
  return ['where-check', location] as const
}

/**
 * The WHERE step's pre-flight check (stories 26/27): validates `location` is writable and
 * pre-checks the Backup-dir collision against the *same* name `backup()` will later compute.
 * Disabled while no `location` has been chosen yet — there is nothing to check before the user
 * has picked one.
 */
export function useWhereCheck(location: string | null) {
  return useQuery({
    queryKey: whereCheckQueryKey(location ?? ''),
    queryFn: () => invoke<WhereCheckDto>('where_check', { location }),
    enabled: location !== null && location.length > 0,
    // A collision or writability fault is a fact of the filesystem right now, not something to
    // retry away — retrying only delays the user learning about a real refusal.
    retry: false,
  })
}
