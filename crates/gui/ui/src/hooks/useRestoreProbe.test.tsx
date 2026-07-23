import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { RestoreProbeDto } from '@/types/restore'

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { useRestoreProbe } from './useRestoreProbe'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

describe('useRestoreProbe', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('stays disabled (no invoke) while no backup dir has been picked', () => {
    const { result } = renderHook(() => useRestoreProbe(null), { wrapper })

    expect(result.current.fetchStatus).toBe('idle')
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('reports an encrypted backup once a dir is picked', async () => {
    const dto: RestoreProbeDto = { exists: true, encrypted: true }
    mockedInvoke.mockResolvedValue(dto)

    const { result } = renderHook(() => useRestoreProbe('/backups/x'), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(result.current.data).toEqual(dto)
    expect(mockedInvoke).toHaveBeenCalledWith('restore_probe', { backupDir: '/backups/x' })
  })

  it('reports a plaintext backup', async () => {
    const dto: RestoreProbeDto = { exists: true, encrypted: false }
    mockedInvoke.mockResolvedValue(dto)

    const { result } = renderHook(() => useRestoreProbe('/backups/plain'), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(result.current.data?.encrypted).toBe(false)
  })
})
