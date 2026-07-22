import { useQuery } from '@tanstack/react-query'

import { invoke } from '@/lib/ipc'
import type { TreeNodeDto } from '@/types/tree'

/**
 * The lazy `["tree", prefix]` query key (ADR-0008 D2 "Views out", issue #100). `category:
 * null` is the root level (one node per Category, Hide-empty applied); `category:
 * Some(name)` is that Category's own directory/file children under `path`.
 *
 * Exported so a mutation invalidates the whole family with the leading `['tree']` tuple
 * (`treeRootQueryKey`) — TanStack's prefix-match invalidation — rather than a single node's
 * key: a Category toggle or a Subtree deselection can change tri-state at any depth,
 * mounted or not, so a mutation invalidates broadly and only what's actually mounted
 * refetches (the "invalidate, don't return" convention).
 */
export function treeQueryKey(category: string | null, path: string | null) {
  return ['tree', category, path] as const
}

/** The prefix every `treeQueryKey(...)` tuple starts with — what a mutation invalidates. */
export const treeRootQueryKey = ['tree'] as const

/** The tree's root level: one node per Category (Hide-empty applied, Custom paths always
 * shown). */
export function useTreeRoot() {
  return useQuery({
    queryKey: treeQueryKey(null, null),
    queryFn: () => invoke<TreeNodeDto[]>('tree', { category: null, path: null }),
  })
}

/**
 * `category`'s own directory/file children under `path` (`null` meaning the Category's own
 * top level) — the lazy per-directory descent a tree node's expand fetches.
 */
export function useTreeChildren(category: string, path: string | null) {
  return useQuery({
    queryKey: treeQueryKey(category, path),
    queryFn: () => invoke<TreeNodeDto[]>('tree', { category, path }),
  })
}
