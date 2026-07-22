import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

import { SelectionSummary } from '@/components/SelectionSummary'

const queryClient = new QueryClient()

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <main className="mx-auto flex min-h-svh max-w-lg flex-col gap-6 p-8">
        <h1 className="text-xl font-semibold">linux-backup</h1>
        <SelectionSummary />
      </main>
    </QueryClientProvider>
  )
}

export default App
