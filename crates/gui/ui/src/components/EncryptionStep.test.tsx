import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { PropsWithChildren } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { EncryptionStatusDto } from '@/types/encryption-status'

// The frontend seam (spec #97's Testing Decisions): mock exactly the IPC boundary module
// (`@/lib/ipc`), never `@tauri-apps/api/core` directly.
vi.mock('@/lib/ipc', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@/lib/ipc'

import { EncryptionStep } from './EncryptionStep'

const mockedInvoke = vi.mocked(invoke)

function wrapper({ children }: PropsWithChildren) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

const sensitiveStatus: EncryptionStatusDto = {
  autoInsistRequired: true,
  sensitiveCategories: ['SSH', 'GPG'],
  hasPassphrase: false,
  isComplete: false,
  isInvalidated: false,
}

/**
 * A focused test of story 25's safety-critical path: overriding Auto-insist for a Sensitive
 * Selection must go through a confirmation that names the Categories about to land in
 * plaintext and stays disabled until the literal word `PLAINTEXT` is typed — never a single
 * click. This pins the invariant so a future refactor can't silently turn the override into
 * a one-click opt-out. Deliberately narrow (one component, one interaction path) per #97's
 * Testing Decisions — not broad render/snapshot coverage.
 */
describe('EncryptionStep — plaintext override gate (story 25)', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    mockedInvoke.mockResolvedValue(sensitiveStatus)
  })

  it('requires the literal PLAINTEXT word before Confirm enables, and names the Sensitive Categories', async () => {
    render(<EncryptionStep />, { wrapper })

    const openModal = await screen.findByRole('button', { name: 'Disable encryption (plaintext)' })
    // Auto-insist required: the direct one-click "Leave plaintext" affordance must not exist —
    // only the named, gated override does.
    expect(screen.queryByRole('button', { name: 'Leave plaintext' })).not.toBeInTheDocument()

    fireEvent.click(openModal)

    const dialog = await screen.findByRole('dialog', { name: 'Write credentials in plaintext?' })
    expect(dialog).toHaveTextContent('SSH')
    expect(dialog).toHaveTextContent('GPG')

    const confirmButton = screen.getByRole('button', { name: 'Confirm' })
    expect(confirmButton).toBeDisabled()

    const confirmInput = screen.getByRole('textbox')

    fireEvent.change(confirmInput, { target: { value: 'plaintext' } })
    expect(confirmButton).toBeDisabled()

    fireEvent.change(confirmInput, { target: { value: 'PLAINTEXT' } })
    await waitFor(() => expect(confirmButton).not.toBeDisabled())
  })
})
