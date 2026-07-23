import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { WhereCheckDto } from '@/types/where-check'

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { useHomeDirectory, useWhereCheck, whereCheckQueryKey } from './useWhere'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

describe('useHomeDirectory', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invokes home_directory', async () => {
    mockedInvoke.mockResolvedValue('/home/tester')

    const { result } = renderHook(() => useHomeDirectory(), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(result.current.data).toBe('/home/tester')
    expect(mockedInvoke).toHaveBeenCalledWith('home_directory')
  })
})

describe('useWhereCheck', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('stays disabled (no invoke) while no location has been chosen', () => {
    const { result } = renderHook(() => useWhereCheck(null), { wrapper })

    expect(result.current.fetchStatus).toBe('idle')
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('resolves the pre-check once a location is chosen', async () => {
    const dto: WhereCheckDto = {
      location: '/home/tester',
      backupDir: '/home/tester/linux-backup-host-1',
    }
    mockedInvoke.mockResolvedValue(dto)

    const { result } = renderHook(() => useWhereCheck('/home/tester'), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(result.current.data).toEqual(dto)
    expect(mockedInvoke).toHaveBeenCalledWith('where_check', { location: '/home/tester' })
    expect(whereCheckQueryKey('/home/tester')).toEqual(['where-check', '/home/tester'])
  })

  it('surfaces a Backup-dir collision as the query error state', async () => {
    mockedInvoke.mockRejectedValue({
      kind: 'backup_dir_exists',
      message: 'backup dir already exists: /home/tester/linux-backup-host-1',
    })

    const { result } = renderHook(() => useWhereCheck('/home/tester'), { wrapper })

    await waitFor(() => expect(result.current.isError).toBe(true))
    expect(result.current.error).toEqual({
      kind: 'backup_dir_exists',
      message: 'backup dir already exists: /home/tester/linux-backup-host-1',
    })
  })
})
