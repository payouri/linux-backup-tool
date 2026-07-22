import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { SelectionSummaryDto } from '@/types/selection-summary'
import type { TreeNodeDto } from '@/types/tree'

// The frontend seam (spec #97's Testing Decisions): mock exactly the IPC boundary module
// (`@/lib/ipc`), never `@tauri-apps/api/core` directly, so a mutation's `onSuccess`
// invalidation is exercised against TanStack Query's real cache, not a stubbed one.
vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { useSelectionSummary } from './useSelectionSummary'
import {
  useAddCustomPath,
  useDeselectRoot,
  useReselectRoot,
  useSelectCategory,
} from './useSelectionMutations'
import { useTreeChildren, useTreeRoot } from './useTree'

const mockedInvoke = vi.mocked(invoke)

function makeWrapper(queryClient: QueryClient) {
  return function Wrapper({ children }: PropsWithChildren) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  }
}

function newQueryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } })
}

const kwinrcNode: TreeNodeDto = {
  id: 'category:KDE Plasma:kwinrc',
  name: 'kwinrc',
  category: 'KDE Plasma',
  relativePath: 'kwinrc',
  kind: 'file',
  state: 'all',
  hasChildren: false,
  addAffordance: false,
}

const iconsNodeAll: TreeNodeDto = {
  ...kwinrcNode,
  id: 'category:KDE Plasma:icons',
  name: 'icons',
  relativePath: 'icons',
  kind: 'directory',
  state: 'all',
  hasChildren: true,
}

const iconsNodePartial: TreeNodeDto = { ...iconsNodeAll, state: 'partial' }

const summaryBefore: SelectionSummaryDto = {
  entryCount: 3,
  totalSizeBytes: 300,
  isEmpty: false,
  encryptionRequired: false,
  warnings: [],
}

const summaryAfter: SelectionSummaryDto = { ...summaryBefore, entryCount: 2, totalSizeBytes: 200 }

// Every hook below the mounted views a mutation is expected to invalidate, plus the
// mutation itself, are rendered together in ONE `renderHook` call (one React root, sharing
// the one `QueryClient`) — exactly how the real app mounts them (a screen's hooks compose
// in one component tree). Splitting them across separate `renderHook` roots is an easy trap
// in this harness: cross-root query-cache notifications land, but the *other* root's own
// render commit can lag independently of `waitFor`'s polling, so its `result.current` can
// still read stale after the cache has already moved on — a test-harness artifact, not a
// product behavior, that combining the hooks into one render root avoids entirely.
describe('a Subtree mutation invalidates every mounted tree query and the selection summary', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('refetches a mounted useTreeChildren view and useSelectionSummary after deselect_root', async () => {
    const queryClient = newQueryClient()
    const wrapper = makeWrapper(queryClient)

    mockedInvoke.mockResolvedValueOnce([kwinrcNode, iconsNodeAll])
    mockedInvoke.mockResolvedValueOnce(summaryBefore)

    const combined = renderHook(
      () => ({
        tree: useTreeChildren('KDE Plasma', null),
        summary: useSelectionSummary(),
        mutation: useDeselectRoot(),
      }),
      { wrapper },
    )

    await waitFor(() => expect(combined.result.current.tree.isSuccess).toBe(true))
    await waitFor(() => expect(combined.result.current.summary.isSuccess).toBe(true))
    expect(combined.result.current.tree.data).toEqual([kwinrcNode, iconsNodeAll])

    // The mutation call itself, then the two refetches its `onSuccess` invalidation drives.
    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce([kwinrcNode, iconsNodePartial])
    mockedInvoke.mockResolvedValueOnce(summaryAfter)

    combined.result.current.mutation.mutate('icons/heavy.svg')

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('deselect_root', { root: 'icons/heavy.svg' })

    await waitFor(() =>
      expect(combined.result.current.tree.data).toEqual([kwinrcNode, iconsNodePartial]),
    )
    await waitFor(() => expect(combined.result.current.summary.data).toEqual(summaryAfter))
  })

  it('also invalidates the mounted tree view after reselect_root', async () => {
    const queryClient = newQueryClient()
    const wrapper = makeWrapper(queryClient)

    mockedInvoke.mockResolvedValueOnce([iconsNodePartial])

    const combined = renderHook(
      () => ({
        tree: useTreeChildren('KDE Plasma', null),
        mutation: useReselectRoot(),
      }),
      { wrapper },
    )
    await waitFor(() => expect(combined.result.current.tree.isSuccess).toBe(true))

    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce([iconsNodeAll])

    combined.result.current.mutation.mutate('icons/heavy.svg')

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('reselect_root', { root: 'icons/heavy.svg' })
    await waitFor(() => expect(combined.result.current.tree.data).toEqual([iconsNodeAll]))
  })

  it('invalidates a mounted useTreeRoot view after select_category', async () => {
    const queryClient = newQueryClient()
    const wrapper = makeWrapper(queryClient)
    const categoryOff: TreeNodeDto = {
      id: 'category:User data',
      name: 'User data',
      category: 'User data',
      relativePath: null,
      kind: 'category',
      state: 'none',
      hasChildren: true,
      addAffordance: false,
    }
    const categoryOn: TreeNodeDto = { ...categoryOff, state: 'all' }

    mockedInvoke.mockResolvedValueOnce([categoryOff])

    const combined = renderHook(
      () => ({
        root: useTreeRoot(),
        mutation: useSelectCategory(),
      }),
      { wrapper },
    )
    await waitFor(() => expect(combined.result.current.root.isSuccess).toBe(true))

    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce([categoryOn])

    combined.result.current.mutation.mutate('User data')

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('select_category', { category: 'User data' })
    await waitFor(() => expect(combined.result.current.root.data).toEqual([categoryOn]))
  })
})

describe('useAddCustomPath', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('surfaces a rejected custom path as the mutation error state, not a thrown crash', async () => {
    const queryClient = newQueryClient()
    const wrapper = makeWrapper(queryClient)
    mockedInvoke.mockRejectedValue({
      kind: 'custom_path_outside_home',
      message: 'path /etc/nixos/configuration.nix is outside $HOME /home/tester',
    })

    const { result } = renderHook(() => useAddCustomPath(), { wrapper })
    result.current.mutate('/etc/nixos/configuration.nix')

    await waitFor(() => expect(result.current.isError).toBe(true))
    expect(result.current.error).toEqual({
      kind: 'custom_path_outside_home',
      message: 'path /etc/nixos/configuration.nix is outside $HOME /home/tester',
    })
  })

  it('invalidates the selection summary once a custom path is accepted', async () => {
    const queryClient = newQueryClient()
    const wrapper = makeWrapper(queryClient)

    mockedInvoke.mockResolvedValueOnce(summaryBefore)

    const combined = renderHook(
      () => ({
        summary: useSelectionSummary(),
        mutation: useAddCustomPath(),
      }),
      { wrapper },
    )
    await waitFor(() => expect(combined.result.current.summary.isSuccess).toBe(true))

    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce(summaryAfter)

    combined.result.current.mutation.mutate('/home/tester/notes/todo.txt')

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('add_custom_path', {
      absolutePath: '/home/tester/notes/todo.txt',
    })
    await waitFor(() => expect(combined.result.current.summary.data).toEqual(summaryAfter))
  })
})
