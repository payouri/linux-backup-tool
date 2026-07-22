import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { TreeNodeDto } from '@/types/tree'

// The frontend seam (spec #97's Testing Decisions): mock exactly the IPC boundary module
// (`@/lib/ipc`), never `@tauri-apps/api/core` directly.
vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { treeQueryKey, treeRootQueryKey, useTreeChildren, useTreeRoot } from './useTree'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

const categoryRoot: TreeNodeDto[] = [
  {
    id: 'category:SSH',
    name: 'SSH',
    category: 'SSH',
    relativePath: null,
    kind: 'category',
    state: 'all',
    hasChildren: true,
    addAffordance: false,
  },
]

const kdeChildren: TreeNodeDto[] = [
  {
    id: 'category:KDE Plasma:icons',
    name: 'icons',
    category: 'KDE Plasma',
    relativePath: 'icons',
    kind: 'directory',
    state: 'partial',
    hasChildren: true,
    addAffordance: false,
  },
]

describe('treeQueryKey', () => {
  it('is a tuple prefixed by the root key, for prefix-match invalidation', () => {
    expect(treeQueryKey(null, null)).toEqual(['tree', null, null])
    expect(treeQueryKey('KDE Plasma', 'icons')).toEqual(['tree', 'KDE Plasma', 'icons'])
    expect(treeRootQueryKey).toEqual(['tree'])
  })
})

describe('useTreeRoot', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invokes the tree command with no category/path for the root level', async () => {
    mockedInvoke.mockResolvedValue(categoryRoot)

    const { result } = renderHook(() => useTreeRoot(), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data).toEqual(categoryRoot)
    expect(mockedInvoke).toHaveBeenCalledWith('tree', { category: null, path: null })
  })
})

describe('useTreeChildren', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invokes the tree command scoped to the given category and path', async () => {
    mockedInvoke.mockResolvedValue(kdeChildren)

    const { result } = renderHook(() => useTreeChildren('KDE Plasma', null), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data).toEqual(kdeChildren)
    expect(mockedInvoke).toHaveBeenCalledWith('tree', { category: 'KDE Plasma', path: null })
  })

  it('reflects a mixed subtree as the partial tri-state the mocked command returns', async () => {
    mockedInvoke.mockResolvedValue(kdeChildren)

    const { result } = renderHook(() => useTreeChildren('KDE Plasma', null), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data?.[0].state).toBe('partial')
  })
})
