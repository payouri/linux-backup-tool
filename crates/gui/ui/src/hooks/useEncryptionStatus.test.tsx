import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { EncryptionStatusDto } from '@/types/encryption-status'

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { encryptionStatusQueryKey, useEncryptionStatus } from './useEncryptionStatus'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

const sensitiveStatus: EncryptionStatusDto = {
  autoInsistRequired: true,
  sensitiveCategories: ['SSH'],
  hasPassphrase: false,
  isComplete: false,
  isInvalidated: false,
}

describe('useEncryptionStatus', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invokes encryption_status under the encryption-status query key', async () => {
    mockedInvoke.mockResolvedValue(sensitiveStatus)

    const { result } = renderHook(() => useEncryptionStatus(), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data).toEqual(sensitiveStatus)
    expect(mockedInvoke).toHaveBeenCalledWith('encryption_status')
    expect(encryptionStatusQueryKey).toEqual(['encryption-status'])
  })
})
