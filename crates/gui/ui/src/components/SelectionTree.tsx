import { useEffect, useRef, useState } from 'react'
import { Tree, type NodeRendererProps, type TreeApi } from 'react-arborist'

import { Button } from '@/components/ui/button'
import { ChildFetcher } from '@/components/ChildFetcher'
import { useTreeRoot } from '@/hooks/useTree'
import {
  useAddCustomPath,
  useDeselectCategory,
  useDeselectEntry,
  useDeselectRoot,
  useReselectEntry,
  useReselectRoot,
  useSelectCategory,
} from '@/hooks/useSelectionMutations'
import { LOADING_PLACEHOLDER_SUFFIX, useLazyTreeData, type TreeRow } from '@/hooks/useLazyTreeData'
import { cn } from '@/lib/utils'
import type { TreeNodeDto } from '@/types/tree'

/** The `$HOME`-relative path a mutation command takes for `dto` (`null` for a Category's own
 * top level, `useTreeChildren`'s `path: null`). */
function pathQueryFor(dto: TreeNodeDto): string | null {
  return dto.relativePath
}

/** The tri-state checkbox: `[x]`/`[~]`/`[ ]`, mirroring `crates/tui`'s own glyphs (CONTEXT.md
 * "Partial selection"). A native checkbox's `indeterminate` DOM property (not a prop React
 * exposes directly) renders the `Partial` state. */
function TriStateCheckbox({
  dto,
  onToggle,
}: {
  dto: TreeNodeDto
  onToggle: () => void
}) {
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (ref.current) {
      ref.current.indeterminate = dto.state === 'partial'
    }
  }, [dto.state])
  return (
    <input
      ref={ref}
      type="checkbox"
      className="size-4 shrink-0"
      checked={dto.state === 'all'}
      onChange={onToggle}
      onClick={(event) => event.stopPropagation()}
      aria-label={`Toggle ${dto.name}`}
    />
  )
}

/** The always-shown Custom-paths add affordance (CONTEXT.md story 6): a plain text input
 * plus a submit button. A rejected path surfaces `add_custom_path`'s typed
 * `SerializableCoreError` message inline rather than crashing (issue #100). */
function CustomPathAdd() {
  const [buffer, setBuffer] = useState('')
  const addCustomPath = useAddCustomPath()

  function submit() {
    const trimmed = buffer.trim()
    if (!trimmed) {
      return
    }
    addCustomPath.mutate(trimmed, {
      onSuccess: () => setBuffer(''),
    })
  }

  return (
    <div className="ml-2 flex flex-1 items-center gap-2" onClick={(event) => event.stopPropagation()}>
      <input
        value={buffer}
        onChange={(event) => setBuffer(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') {
            submit()
          }
        }}
        placeholder="/path/to/add"
        className="h-6 min-w-0 flex-1 rounded border border-border bg-background px-1.5 text-xs"
      />
      <Button variant="outline" size="xs" onClick={submit} disabled={addCustomPath.isPending}>
        Add
      </Button>
      {addCustomPath.isError && (
        <span role="alert" className="text-destructive text-xs">
          {addCustomPath.error.message}
        </span>
      )}
    </div>
  )
}

function Node({ node, style }: NodeRendererProps<TreeRow>) {
  const { dto } = node.data
  const selectCategory = useSelectCategory()
  const deselectCategory = useDeselectCategory()
  const deselectEntry = useDeselectEntry()
  const reselectEntry = useReselectEntry()
  const deselectRoot = useDeselectRoot()
  const reselectRoot = useReselectRoot()

  function toggleSelection() {
    const isChecked = dto.state === 'all'
    if (dto.kind === 'category') {
      if (isChecked) {
        deselectCategory.mutate(dto.category)
      } else {
        selectCategory.mutate(dto.category)
      }
      return
    }
    const relativePath = pathQueryFor(dto)
    if (!relativePath) {
      return
    }
    if (dto.kind === 'directory') {
      if (isChecked) {
        deselectRoot.mutate(relativePath)
      } else {
        reselectRoot.mutate(relativePath)
      }
    } else {
      if (isChecked) {
        deselectEntry.mutate(relativePath)
      } else {
        reselectEntry.mutate(relativePath)
      }
    }
  }

  const isLoadingPlaceholder = dto.id.endsWith(LOADING_PLACEHOLDER_SUFFIX)

  if (isLoadingPlaceholder) {
    return (
      <div style={style} className="flex h-full items-center gap-1.5 px-1.5 text-sm">
        <span className="w-3 shrink-0" />
        <span className="w-4 shrink-0" />
        <span className="text-muted-foreground italic">{dto.name}</span>
      </div>
    )
  }

  return (
    <div
      style={style}
      className={cn(
        'flex h-full items-center gap-1.5 px-1.5 text-sm',
        node.isSelected && 'bg-muted',
      )}
      onClick={() => node.isInternal && node.toggle()}
    >
      {node.isInternal ? (
        <span className="w-3 shrink-0 text-muted-foreground">{node.isOpen ? '▾' : '▸'}</span>
      ) : (
        <span className="w-3 shrink-0" />
      )}
      <TriStateCheckbox dto={dto} onToggle={toggleSelection} />
      <span className="truncate">{dto.name}</span>
      {dto.addAffordance && <CustomPathAdd />}
    </div>
  )
}

/**
 * The WHAT step's explorer tree (issue #100): the root level is one row per Category
 * (Hide-empty applied, Custom paths always shown); expanding a Category or directory lazily
 * fetches its children via `["tree", prefix]`. Checking a row routes to the matching
 * mutation (`select_category`/`deselect_category` for a Category row,
 * `deselect_root`/`reselect_root` for a directory, `deselect_entry`/`reselect_entry` for a
 * file) — the frontend never re-derives ADR-0006's prefix-wins rule itself, only reflects
 * `core`'s own `directory_state` verdict.
 *
 * `react-arborist` (ADR-0008's pre-built pick) gives virtualization, multi-select, and
 * keyboard navigation out of the box. Lazy loading and keeping already-expanded levels live
 * for invalidation-triggered refetches (issue #100 acceptance criterion 2 / story 20) are
 * [`useLazyTreeData`] and [`ChildFetcher`]'s job — see their docs for why and how.
 */
export function SelectionTree() {
  const { data: rootNodes, isPending, error, refetch } = useTreeRoot()
  const { data, pendingFetches, markLoaded, setOpen } = useLazyTreeData(rootNodes)
  const treeApiRef = useRef<TreeApi<TreeRow> | undefined>(undefined)

  function handleToggle(id: string) {
    const node = treeApiRef.current?.get(id)
    setOpen(id, node?.isOpen ?? false)
  }

  if (isPending) {
    return <p className="text-muted-foreground text-sm">Scanning your Environment…</p>
  }

  if (error) {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-destructive text-sm" role="alert">
          Could not load the Category tree: {error.message}
        </p>
        <Button variant="outline" size="sm" onClick={() => refetch()}>
          Retry
        </Button>
      </div>
    )
  }

  return (
    <div className="h-[28rem] rounded-md border border-border">
      {pendingFetches.map((dto) => (
        <ChildFetcher
          key={dto.id}
          category={dto.category}
          path={pathQueryFor(dto)}
          onLoaded={(children) => markLoaded(dto.id, children)}
        />
      ))}
      <Tree<TreeRow>
        ref={treeApiRef}
        data={data}
        idAccessor={(row) => row.dto.id}
        childrenAccessor={(row) => row.children}
        width="100%"
        height={448}
        rowHeight={28}
        indent={18}
        disableDrag
        disableEdit
        onToggle={handleToggle}
      >
        {Node}
      </Tree>
    </div>
  )
}
