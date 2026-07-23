import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

import { EncryptionStep } from '@/components/EncryptionStep'
import { RunScreen } from '@/components/RunScreen'
import { SelectionSummary } from '@/components/SelectionSummary'
import { SelectionTree } from '@/components/SelectionTree'
import { WhereStep } from '@/components/WhereStep'
import { WizardStepper } from '@/components/WizardStepper'
import { useWizardStepper } from '@/hooks/useWizardStepper'

const queryClient = new QueryClient()

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

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <main className="mx-auto flex min-h-svh max-w-2xl flex-col gap-6 p-8">
        <h1 className="text-xl font-semibold">linux-backup</h1>
        <WizardStepper>
          <WizardBody />
        </WizardStepper>
      </main>
    </QueryClientProvider>
  )
}

export default App
