import { useState } from 'react'
import { QueryClient, QueryClientProvider, useIsMutating } from '@tanstack/react-query'

import { Button } from '@/components/ui/button'
import { EncryptionStep } from '@/components/EncryptionStep'
import { RestoreScreen } from '@/components/RestoreScreen'
import { RunScreen } from '@/components/RunScreen'
import { SelectionSummary } from '@/components/SelectionSummary'
import { SelectionTree } from '@/components/SelectionTree'
import { WhereStep } from '@/components/WhereStep'
import { WizardStepper } from '@/components/WizardStepper'
import { useWizardStepper } from '@/hooks/useWizardStepper'

const queryClient = new QueryClient()

/** Which top-level flow the window shows (issue #103): Restore is its own route, not a wizard
 * step — it opens by picking a Backup dir directly, mirroring `crates/tui`'s standalone
 * restore route rather than living inside the WHAT→encryption→WHERE→review stepper. */
type Mode = 'backup' | 'restore'

/** Renders the current wizard step's body (issue #101: WHAT was already wired by #100; this
 * ticket adds encryption and WHERE, plus the stepper shell around all three). Issue #102 fills
 * in REVIEW: launching the backup and watching it run (`RunScreen`). */
function WizardBody() {
  const { currentStep } = useWizardStepper()

  switch (currentStep) {
    case 'what':
      return (
        <div className="flex flex-col gap-6">
          <SelectionTree />
          <SelectionSummary />
        </div>
      )
    case 'encryption':
      return <EncryptionStep />
    case 'where':
      return <WhereStep />
    case 'review':
      return <RunScreen />
  }
}

/**
 * The window body (issue #103 review fix): split out from `App` so it renders *inside*
 * `QueryClientProvider` — `useIsMutating` needs the query client context, which isn't
 * available to hooks called directly in the component that itself instantiates the Provider.
 * Disables (rather than merely visually de-emphasizing) the Backup/Restore toggle while
 * either run's mutation is in flight, so switching tabs mid-run — which would unmount the
 * active screen's `useBackupRun`/`useRestore` hook instance without ever resolving a pending
 * collision prompt, permanently stranding that worker thread on its reply channel — isn't
 * reachable through the normal UI at all. The backend guard (`try_begin_run`,
 * `SerializableCoreError::already_running`) is the actual enforcement; this is the
 * UI-hardening layer on top of it.
 */
function AppContent() {
  const [mode, setMode] = useState<Mode>('backup')
  const backupRunning = useIsMutating({ mutationKey: ['run-backup'] }) > 0
  const restoreRunning = useIsMutating({ mutationKey: ['run-restore'] }) > 0
  const runInProgress = backupRunning || restoreRunning

  return (
    <main className="mx-auto flex min-h-svh max-w-2xl flex-col gap-6 p-8">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold">linux-backup</h1>
        <div className="flex gap-1">
          <Button
            variant={mode === 'backup' ? 'default' : 'outline'}
            size="sm"
            disabled={runInProgress}
            onClick={() => setMode('backup')}
          >
            Backup
          </Button>
          <Button
            variant={mode === 'restore' ? 'default' : 'outline'}
            size="sm"
            disabled={runInProgress}
            onClick={() => setMode('restore')}
          >
            Restore
          </Button>
        </div>
      </div>
      {mode === 'backup' ? (
        <WizardStepper>
          <WizardBody />
        </WizardStepper>
      ) : (
        <RestoreScreen />
      )}
    </main>
  )
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <AppContent />
    </QueryClientProvider>
  )
}

export default App
