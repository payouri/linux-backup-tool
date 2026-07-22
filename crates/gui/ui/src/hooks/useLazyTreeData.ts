import { useMemo, useState } from 'react'

import type { TreeNodeDto } from '@/types/tree'

/** The synthetic id suffix marking [`loadingPlaceholderRow`]'s row — never a real
 * `TreeNodeDto.id` (those are always `category:...`-prefixed), so a plain suffix check
 * distinguishes it in the renderer without a dedicated "kind". */
export const LOADING_PLACEHOLDER_SUFFIX = '::loading'

/**
 * One row react-arborist renders. `children: null` is a leaf (a File, an empty directory,
 * or the empty Custom-paths Category). An unexpanded internal node whose real children
 * haven't been fetched yet gets a single [`loadingPlaceholderRow`] child instead of `null`
 * — react-arborist's `childrenAccessor` only distinguishes "leaf" (`null`) from "has
 * children" (an array), with no third "unknown yet" state of its own, so a synthetic
 * placeholder child is what keeps the row expandable *before* a lazy fetch resolves its
 * real children (issue #100: "loads lazily per expanded directory", never eagerly).
 */
export interface TreeRow {
  dto: TreeNodeDto
  children: TreeRow[] | null
}

function loadingPlaceholderRow(parentId: string): TreeRow {
  return {
    dto: {
      id: `${parentId}${LOADING_PLACEHOLDER_SUFFIX}`,
      name: 'Loading…',
      category: '',
      relativePath: null,
      kind: 'file',
      state: 'none',
      hasChildren: false,
      addAffordance: false,
    },
    children: null,
  }
}

function buildRow(dto: TreeNodeDto, loaded: ReadonlyMap<string, TreeNodeDto[]>): TreeRow {
  if (!dto.hasChildren) {
    return { dto, children: null }
  }
  const childDtos = loaded.get(dto.id)
  if (childDtos === undefined) {
    return { dto, children: [loadingPlaceholderRow(dto.id)] }
  }
  return { dto, children: childDtos.map((child) => buildRow(child, loaded)) }
}

export interface LazyTreeData {
  /** The rows react-arborist renders, rebuilt from `rootNodes` plus whatever has been
   * fetched into `loaded` so far. */
  data: TreeRow[]
  /**
   * Every currently OPEN node with children — the set a `ChildFetcher` must stay mounted
   * for, regardless of whether it has already loaded once.
   *
   * This is the fix for the mount/unmount-after-load bug an earlier build shipped: filtering
   * this list to `!loaded.has(id)` meant each `ChildFetcher` — the sole holder of that
   * node's `useTreeChildren` observer — unmounted the instant its first fetch resolved.
   * `invalidateQueries({queryKey: ['tree']})`'s default `refetchType: 'active'` only
   * refetches queries with a *live* observer, so once unmounted, an already-expanded child
   * level would never refetch again — a Category row's own tri-state (driven by the
   * always-mounted `useTreeRoot`) would update after a Drill-down mutation, but every
   * expanded level below it would keep rendering whatever `loaded` snapshot it froze on,
   * forever (collapse/reopen didn't help either, since `loaded` still held the stale
   * entry). Keeping every OPEN node's fetcher mounted for as long as it stays open — not
   * just until its first successful load — is what lets a later invalidation's refetch
   * flow back in via `markLoaded` overwriting the same entry.
   */
  pendingFetches: TreeNodeDto[]
  /** Records (or overwrites) `id`'s fetched children — called on every `data` change from
   * the `ChildFetcher` still watching it, not just the first. */
  markLoaded: (id: string, children: TreeNodeDto[]) => void
  /** Tracks `id`'s real open/closed state (react-arborist's own `onToggle` reports both
   * directions) — `pendingFetches` must shrink back down when a node collapses, both so a
   * closed node's `ChildFetcher` unmounts (no need to keep polling children nobody can see)
   * and so `openIds` doesn't grow unboundedly across a long session. */
  setOpen: (id: string, isOpen: boolean) => void
}

/**
 * Owns the lazy, per-node tree data `SelectionTree` renders: which nodes are open, what's
 * been fetched for them so far, and the resulting react-arborist `data` array. Kept as its
 * own hook (rather than inline `useState`/`useMemo` in the component) so the
 * mount/unmount-after-load behavior above is testable directly against a small harness,
 * without needing to drive the real virtualized `<Tree>` widget's DOM.
 */
export function useLazyTreeData(rootNodes: TreeNodeDto[] | undefined): LazyTreeData {
  const [openIds, setOpenIds] = useState<ReadonlySet<string>>(new Set())
  const [loaded, setLoaded] = useState<ReadonlyMap<string, TreeNodeDto[]>>(new Map())

  const data = useMemo(
    () => (rootNodes ?? []).map((dto) => buildRow(dto, loaded)),
    [rootNodes, loaded],
  )

  const dtoById = useMemo(() => {
    const map = new Map<string, TreeNodeDto>()
    const visit = (rows: TreeRow[]) => {
      for (const row of rows) {
        map.set(row.dto.id, row.dto)
        if (row.children) {
          visit(row.children)
        }
      }
    }
    visit(data)
    return map
  }, [data])

  const pendingFetches = useMemo(
    () =>
      [...openIds]
        .map((id) => dtoById.get(id))
        .filter((dto): dto is TreeNodeDto => dto !== undefined && dto.hasChildren),
    [openIds, dtoById],
  )

  function markLoaded(id: string, children: TreeNodeDto[]) {
    setLoaded((prev) => {
      const next = new Map(prev)
      next.set(id, children)
      return next
    })
  }

  function setOpen(id: string, isOpen: boolean) {
    setOpenIds((prev) => {
      const next = new Set(prev)
      if (isOpen) {
        next.add(id)
      } else {
        next.delete(id)
      }
      return next
    })
  }

  return { data, pendingFetches, markLoaded, setOpen }
}
