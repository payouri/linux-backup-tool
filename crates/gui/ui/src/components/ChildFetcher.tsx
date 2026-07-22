import { useEffect, useRef } from 'react'

import { useTreeChildren } from '@/hooks/useTree'
import type { TreeNodeDto } from '@/types/tree'

/**
 * Fetches one open node's children via the lazy `["tree", prefix]` query and reports them
 * up every time the query's data changes — not just the first successful fetch. Renders
 * nothing: it exists purely to hang a live `useTreeChildren` observer off an
 * individually-expanded node, since react-arborist itself renders from one in-memory `data`
 * array rather than an async per-node callback (no "lazy load" hook of its own to plug
 * into) — this is the seam that supplies it.
 *
 * Must stay mounted for as long as its node is open (see [`useLazyTreeData`]'s
 * `pendingFetches` doc): an invalidated query only refetches while it still has a live
 * observer, and this component *is* that observer.
 *
 * The effect below depends on `data` alone, **not** `onLoaded` — `onLoaded` is a fresh
 * closure every parent render (`(children) => markLoaded(dto.id, children)` in
 * `SelectionTree`), and `markLoaded` always replaces `loaded` with a new `Map` even when the
 * fetched children are unchanged. Depending on `onLoaded` directly would re-run the effect
 * on every parent render (not just a genuine refetch), call `markLoaded` again, trigger
 * another render, and loop forever. `onLoadedRef` always holds the latest callback without
 * being a dependency itself, so the effect only fires when `useTreeChildren`'s own `data`
 * reference actually changes — a real fetch or refetch, never a re-render echo.
 */
export function ChildFetcher({
  category,
  path,
  onLoaded,
}: {
  category: string
  path: string | null
  onLoaded: (children: TreeNodeDto[]) => void
}) {
  const { data } = useTreeChildren(category, path)
  const onLoadedRef = useRef(onLoaded)
  onLoadedRef.current = onLoaded

  useEffect(() => {
    if (data) {
      onLoadedRef.current(data)
    }
  }, [data])
  return null
}
