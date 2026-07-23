import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { EncryptionStatusDto } from '@/types/encryption-status'

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { useEncryptionStatus } from './useEncryptionStatus'
import { useChoosePlaintext, useSetPassphrase } from './useEncryptionMutations'

const mockedInvoke = vi.mocked(invoke)

function makeWrapper(queryClient: QueryClient) {
  return function Wrapper({ children }: PropsWithChildren) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  }
}

function newQueryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } })
}

const sensitiveIncomplete: EncryptionStatusDto = {
  autoInsistRequired: true,
  sensitiveCategories: ['SSH'],
  hasPassphrase: false,
  isComplete: false,
  isInvalidated: false,
}

const sensitiveWithPassphrase: EncryptionStatusDto = {
  ...sensitiveIncomplete,
  hasPassphrase: true,
  isComplete: true,
}

describe('useSetPassphrase', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('surfaces a blank-passphrase rejection as the mutation error state, not a thrown crash', async () => {
    const queryClient = newQueryClient()
    mockedInvoke.mockRejectedValue({
      kind: 'blank_passphrase',
      message: 'passphrase must not be blank',
    })

    const { result } = renderHook(() => useSetPassphrase(), { wrapper: makeWrapper(queryClient) })
    result.current.mutate('')

    await waitFor(() => expect(result.current.isError).toBe(true))
    expect(result.current.error).toEqual({
      kind: 'blank_passphrase',
      message: 'passphrase must not be blank',
    })
  })

  it('invalidates the mounted encryption-status view once a passphrase is accepted', async () => {
    const queryClient = newQueryClient()
    mockedInvoke.mockResolvedValueOnce(sensitiveIncomplete)

    const combined = renderHook(
      () => ({
        status: useEncryptionStatus(),
        mutation: useSetPassphrase(),
      }),
      { wrapper: makeWrapper(queryClient) },
    )
    await waitFor(() => expect(combined.result.current.status.isSuccess).toBe(true))

    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce(sensitiveWithPassphrase)

    combined.result.current.mutation.mutate('correct-horse-battery-staple9!')

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('set_passphrase', {
      passphrase: 'correct-horse-battery-staple9!',
    })
    await waitFor(() =>
      expect(combined.result.current.status.data).toEqual(sensitiveWithPassphrase),
    )
  })
})

describe('useChoosePlaintext', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
  })

  it('invalidates the mounted encryption-status view after recording a plaintext decision', async () => {
    const queryClient = newQueryClient()
    const nonSensitiveIncomplete: EncryptionStatusDto = {
      autoInsistRequired: false,
      sensitiveCategories: [],
      hasPassphrase: false,
      isComplete: false,
      isInvalidated: false,
    }
    const nonSensitiveComplete: EncryptionStatusDto = { ...nonSensitiveIncomplete, isComplete: true }

    mockedInvoke.mockResolvedValueOnce(nonSensitiveIncomplete)

    const combined = renderHook(
      () => ({
        status: useEncryptionStatus(),
        mutation: useChoosePlaintext(),
      }),
      { wrapper: makeWrapper(queryClient) },
    )
    await waitFor(() => expect(combined.result.current.status.isSuccess).toBe(true))

    mockedInvoke.mockResolvedValueOnce(undefined)
    mockedInvoke.mockResolvedValueOnce(nonSensitiveComplete)

    combined.result.current.mutation.mutate()

    await waitFor(() => expect(combined.result.current.mutation.isSuccess).toBe(true))
    expect(mockedInvoke).toHaveBeenCalledWith('choose_plaintext')
    await waitFor(() => expect(combined.result.current.status.data).toEqual(nonSensitiveComplete))
  })
})
