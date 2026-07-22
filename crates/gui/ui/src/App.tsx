import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

import { SelectionSummary } from '@/components/SelectionSummary'
import { SelectionTree } from '@/components/SelectionTree'

const queryClient = new QueryClient()

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <main className="mx-auto flex min-h-svh max-w-2xl flex-col gap-6 p-8">
        <h1 className="text-xl font-semibold">linux-backup</h1>
        <SelectionTree />
        <SelectionSummary />
      </main>
    </QueryClientProvider>
  )
}

export default App
