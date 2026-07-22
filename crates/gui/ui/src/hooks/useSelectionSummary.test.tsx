import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { SelectionSummaryDto } from '@/types/selection-summary'

// The frontend seam (spec #97's Testing Decisions): mock exactly the IPC boundary module
// (`@/lib/ipc`), never `@tauri-apps/api/core` directly, so hooks run against a stubbed
// command layer rather than a real Tauri runtime.
vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { selectionSummaryQueryKey, useSelectionSummary } from './useSelectionSummary'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

const summary: SelectionSummaryDto = {
  entryCount: 3,
  totalSizeBytes: 2048,
  isEmpty: false,
  encryptionRequired: true,
  warnings: ['User data selection is 1.50 GB — this Category is large and default-OFF for a reason'],
}

describe('useSelectionSummary', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invokes the selection_summary command under the selection-summary query key', async () => {
    mockedInvoke.mockResolvedValue(summary)

    const { result } = renderHook(() => useSelectionSummary(), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data).toEqual(summary)
    expect(mockedInvoke).toHaveBeenCalledWith('selection_summary')
    expect(selectionSummaryQueryKey).toEqual(['selection-summary'])
  })

  it("surfaces a rejected invoke as the query's error state", async () => {
    mockedInvoke.mockRejectedValue(new Error('boom'))

    const { result } = renderHook(() => useSelectionSummary(), { wrapper })

    await waitFor(() => expect(result.current.isError).toBe(true))

    expect(result.current.error).toBeInstanceOf(Error)
  })
})
