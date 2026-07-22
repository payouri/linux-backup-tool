import { QueryClient, QueryClientProvider, useQueryClient } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { TreeNodeDto } from '@/types/tree'

// The frontend seam (spec #97's Testing Decisions): mock exactly the IPC boundary module
// (`@/lib/ipc`), never `@tauri-apps/api/core` directly.
vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { ChildFetcher } from '@/components/ChildFetcher'
import { useLazyTreeData } from './useLazyTreeData'

const mockedInvoke = vi.mocked(invoke)

const rootDto: TreeNodeDto = {
  id: 'category:KDE Plasma',
  name: 'KDE Plasma',
  category: 'KDE Plasma',
  relativePath: null,
  kind: 'category',
  state: 'all',
  hasChildren: true,
  addAffordance: false,
}

const iconsAll: TreeNodeDto = {
  id: 'category:KDE Plasma:icons',
  name: 'icons',
  category: 'KDE Plasma',
  relativePath: 'icons',
  kind: 'directory',
  state: 'all',
  hasChildren: true,
  addAffordance: false,
}

const iconsPartial: TreeNodeDto = { ...iconsAll, state: 'partial' }

/** A minimal harness wiring `useLazyTreeData` + `ChildFetcher` exactly as `SelectionTree`
 * does, minus the real virtualized `<Tree>` widget — driving that widget's DOM in a test is
 * unnecessary (spec #97's Testing Decisions: assert hook/query state, not component render
 * internals) and would obscure the actual bug this test pins down. */
function Harness({ root }: { root: TreeNodeDto }) {
  const { data, pendingFetches, markLoaded, setOpen } = useLazyTreeData([root])
  const firstChild = data[0]?.children?.[0]?.dto

  return (
    <div>
      <button onClick={() => setOpen(root.id, true)}>open</button>
      <button onClick={() => setOpen(root.id, false)}>close</button>
      {pendingFetches.map((dto) => (
        <ChildFetcher
          key={dto.id}
          category={dto.category}
          path={dto.relativePath}
          onLoaded={(children) => markLoaded(dto.id, children)}
        />
      ))}
      <span data-testid="first-child-state">{firstChild?.state ?? 'none-loaded'}</span>
    </div>
  )
}

function InvalidateTreeButton() {
  const queryClient = useQueryClient()
  return (
    <button onClick={() => queryClient.invalidateQueries({ queryKey: ['tree'] })}>
      simulate mutation invalidation
    </button>
  )
}

function wrapper(queryClient: QueryClient) {
  return function Wrapper({ children }: PropsWithChildren) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  }
}

describe('useLazyTreeData + ChildFetcher — the mount/unmount-after-load blocker', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  // `render()` (unlike `renderHook`) mounts DOM the other test files in this project never
  // needed to clean up between tests (vitest isn't configured with `test.globals: true`, so
  // Testing Library's automatic `afterEach(cleanup)` registration never fires) — without
  // this, a second test's `screen.getByText('open')` matches both this test's leftover DOM
  // and its own.
  afterEach(() => {
    cleanup()
  })

  it('refetches an already-expanded child level after invalidation, without closing/reopening it', async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    const Wrapper = wrapper(queryClient)

    mockedInvoke.mockResolvedValueOnce([iconsAll])

    render(
      <Wrapper>
        <Harness root={rootDto} />
        <InvalidateTreeButton />
      </Wrapper>,
    )

    // Open the node: its `ChildFetcher` mounts and the first fetch lands.
    fireEvent.click(screen.getByText('open'))
    await waitFor(() => expect(screen.getByTestId('first-child-state').textContent).toBe('all'))

    // A Drill-down mutation elsewhere invalidates the whole `['tree']` family. The node is
    // still open (never collapsed/reopened) — the bug this regression-tests was that its
    // `ChildFetcher` had already unmounted the instant the first load landed, so this
    // invalidation's refetch had nowhere active to land and the row stayed frozen forever.
    mockedInvoke.mockResolvedValueOnce([iconsPartial])
    fireEvent.click(screen.getByText('simulate mutation invalidation'))

    await waitFor(() =>
      expect(screen.getByTestId('first-child-state').textContent).toBe('partial'),
    )
  })

  it('stops fetching once the node is closed, and resumes fresh data on reopen', async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    const Wrapper = wrapper(queryClient)

    mockedInvoke.mockResolvedValueOnce([iconsAll])

    render(
      <Wrapper>
        <Harness root={rootDto} />
      </Wrapper>,
    )

    fireEvent.click(screen.getByText('open'))
    await waitFor(() => expect(screen.getByTestId('first-child-state').textContent).toBe('all'))
    const callsWhileOpen = mockedInvoke.mock.calls.length

    fireEvent.click(screen.getByText('close'))
    // No further invoke calls fire for a closed node — its `ChildFetcher` has unmounted.
    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(mockedInvoke.mock.calls.length).toBe(callsWhileOpen)

    mockedInvoke.mockResolvedValueOnce([iconsPartial])
    fireEvent.click(screen.getByText('open'))
    await waitFor(() =>
      expect(screen.getByTestId('first-child-state').textContent).toBe('partial'),
    )
  })
})
