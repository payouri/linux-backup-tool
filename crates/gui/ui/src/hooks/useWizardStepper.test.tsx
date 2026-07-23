import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { EncryptionStatusDto } from '@/types/encryption-status'
import type { SelectionSummaryDto } from '@/types/selection-summary'
import type { WhereCheckDto } from '@/types/where-check'

vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { useWizardStore } from '@/stores/useWizardStore'

import { useWizardStepper } from './useWizardStepper'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

const nonEmptySummary: SelectionSummaryDto = {
  entryCount: 1,
  totalSizeBytes: 10,
  isEmpty: false,
  encryptionRequired: false,
  warnings: [],
}

const emptySummary: SelectionSummaryDto = {
  ...nonEmptySummary,
  entryCount: 0,
  isEmpty: true,
}

const nonSensitiveComplete: EncryptionStatusDto = {
  autoInsistRequired: false,
  sensitiveCategories: [],
  hasPassphrase: false,
  isComplete: true,
  isInvalidated: false,
}

const sensitiveIncomplete: EncryptionStatusDto = {
  autoInsistRequired: true,
  sensitiveCategories: ['SSH'],
  hasPassphrase: false,
  isComplete: false,
  isInvalidated: false,
}

const invalidatedPlaintext: EncryptionStatusDto = {
  autoInsistRequired: true,
  sensitiveCategories: ['Custom paths'],
  hasPassphrase: false,
  isComplete: false,
  isInvalidated: true,
}

const whereOk: WhereCheckDto = {
  location: '/home/tester',
  backupDir: '/home/tester/linux-backup-host-1',
}

/** `invoke` is called by three different queries (`selection_summary`, `encryption_status`,
 * `where_check`); this routes each mocked call by command name so a test can hold all three
 * mounted at once without juggling call-order-dependent `mockResolvedValueOnce` chains. */
function routeInvoke(responses: {
  selection_summary?: SelectionSummaryDto
  encryption_status?: EncryptionStatusDto
  where_check?: WhereCheckDto | Error
}) {
  mockedInvoke.mockImplementation((command: string) => {
    if (command === 'selection_summary') {
      return Promise.resolve(responses.selection_summary)
    }
    if (command === 'encryption_status') {
      return Promise.resolve(responses.encryption_status)
    }
    if (command === 'where_check') {
      const result = responses.where_check
      if (result === undefined) {
        // Tests that don't care about WHERE still mount `useWhereCheck` whenever a
        // `whereLocation` happens to be set in the store; reject rather than resolving
        // `undefined` (React Query requires a defined query result either way).
        return Promise.reject(new Error('where_check not stubbed for this test'))
      }
      if (result instanceof Error) {
        return Promise.reject(result)
      }
      return Promise.resolve(result)
    }
    return Promise.reject(new Error(`unexpected command: ${command}`))
  })
}

describe('useWizardStepper', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    // The Zustand store is a module-level singleton (by design, ADR-0008's "client-only
    // global"); reset it between tests so navigation state from one test never leaks into
    // the next.
    useWizardStore.setState({ currentStep: 'what', whereLocation: null })
  })

  it('starts on WHAT with only WHAT reachable when the Selection is empty', async () => {
    routeInvoke({ selection_summary: emptySummary, encryption_status: nonSensitiveComplete })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.stepStatus('what')).toBe('current'))

    expect(result.current.isReachable('what')).toBe(true)
    expect(result.current.isReachable('encryption')).toBe(false)
    expect(result.current.canGoForward).toBe(false)
    expect(result.current.gateReason).toBe('Nothing selected')
  })

  it('advances to encryption once WHAT is complete, and encryption is complete on arrival for a non-Sensitive Selection', async () => {
    routeInvoke({ selection_summary: nonEmptySummary, encryption_status: nonSensitiveComplete })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.canGoForward).toBe(true))

    result.current.goForward()
    await waitFor(() => expect(result.current.currentStep).toBe('encryption'))
    // Non-Sensitive: encryption-complete on arrival (ADR-0007 D4), so forward is already open.
    await waitFor(() => expect(result.current.canGoForward).toBe(true))
  })

  it('gates forward on the encryption step until a Sensitive Selection has a decision', async () => {
    routeInvoke({ selection_summary: nonEmptySummary, encryption_status: sensitiveIncomplete })
    useWizardStore.setState({ currentStep: 'encryption' })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.stepStatus('encryption')).toBe('current'))

    expect(result.current.canGoForward).toBe(false)
    expect(result.current.gateReason).toBe('Enter a passphrase')
    expect(result.current.isReachable('where')).toBe(false)
  })

  it('backward navigation is free and preserves the other steps’ completed status', async () => {
    routeInvoke({ selection_summary: nonEmptySummary, encryption_status: nonSensitiveComplete })
    useWizardStore.setState({ currentStep: 'where', whereLocation: '/home/tester' })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.currentStep).toBe('where'))

    result.current.goBack()
    await waitFor(() => expect(result.current.currentStep).toBe('encryption'))

    // WHAT is still complete and jumpable — going back didn't lose the earlier choice.
    expect(result.current.stepStatus('what')).toBe('completed')
    expect(result.current.isReachable('what')).toBe(true)
  })

  it('a locked step ignores a jump attempt', async () => {
    routeInvoke({ selection_summary: emptySummary, encryption_status: nonSensitiveComplete })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.stepStatus('what')).toBe('current'))

    result.current.jumpTo('where')
    expect(result.current.currentStep).toBe('what')
  })

  it('marks encryption invalidated and reachable when a WHAT edit outgrows a completed plaintext decision', async () => {
    routeInvoke({ selection_summary: nonEmptySummary, encryption_status: invalidatedPlaintext })
    useWizardStore.setState({ currentStep: 'review' })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })

    // The frontier collapses back to encryption (ADR-0007 D5) and pulls `currentStep` off the
    // now-unreachable REVIEW step it was sitting on.
    await waitFor(() => expect(result.current.currentStep).toBe('encryption'))
    expect(result.current.stepStatus('encryption')).toBe('invalidated')
    expect(result.current.isReachable('review')).toBe(false)
  })

  it('WHERE completes once the chosen location resolves the pre-check with no collision', async () => {
    routeInvoke({
      selection_summary: nonEmptySummary,
      encryption_status: nonSensitiveComplete,
      where_check: whereOk,
    })
    useWizardStore.setState({ currentStep: 'where', whereLocation: '/home/tester' })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.canGoForward).toBe(true))

    result.current.goForward()
    await waitFor(() => expect(result.current.currentStep).toBe('review'))
  })

  it('WHERE stays incomplete when the pre-check reports a Backup-dir collision', async () => {
    routeInvoke({
      selection_summary: nonEmptySummary,
      encryption_status: nonSensitiveComplete,
      where_check: Object.assign(new Error('collision'), {
        kind: 'backup_dir_exists',
        message: 'backup dir already exists: /home/tester/linux-backup-host-1',
      }),
    })
    useWizardStore.setState({ currentStep: 'where', whereLocation: '/home/tester' })

    const { result } = renderHook(() => useWizardStepper(), { wrapper })
    await waitFor(() => expect(result.current.whereCheck.isError).toBe(true))

    expect(result.current.canGoForward).toBe(false)
    expect(result.current.gateReason).toBe('Missing backup location')
  })
})
