import { Button } from '@/components/ui/button'
import { useSelectionSummary } from '@/hooks/useSelectionSummary'

/**
 * Renders the live `["selection-summary"]` query (issue #99's walking skeleton): counts,
 * total size, whether Auto-insist requires encryption, and any warnings. The full
 * explorer/tree Selection view (ADR-0006's drill-down) is the per-screen implementation
 * spec's job, not this slice's — this is the first proof that core -> IPC -> TanStack Query
 * -> a rendered window works end to end.
 */
export function SelectionSummary() {
  const { data, error, isPending, isFetching, refetch } = useSelectionSummary()

  if (isPending) {
    return <p className="text-muted-foreground text-sm">Scanning your Environment…</p>
  }

  if (error) {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-destructive text-sm" role="alert">
          Could not load the Selection summary: {error.message}
        </p>
        <Button variant="outline" size="sm" onClick={() => refetch()}>
          Retry
        </Button>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-4">
      <dl className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Entries selected</dt>
        <dd data-testid="entry-count">{data.entryCount}</dd>

        <dt className="text-muted-foreground">Total size</dt>
        <dd data-testid="total-size">{formatBytes(data.totalSizeBytes)}</dd>

        <dt className="text-muted-foreground">Encryption required</dt>
        <dd data-testid="encryption-required">{data.encryptionRequired ? 'Yes' : 'No'}</dd>
      </dl>

      {data.warnings.length > 0 && (
        <ul className="text-sm text-amber-600 dark:text-amber-400">
          {data.warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      )}

      <Button
        variant="outline"
        size="sm"
        onClick={() => refetch()}
        disabled={isFetching}
        className="self-start"
      >
        {isFetching ? 'Refreshing…' : 'Refresh'}
      </Button>
    </div>
  )
}

function formatBytes(bytes: number): string {
  if (bytes === 0) {
    return '0 B'
  }
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const value = bytes / 1024 ** exponent
  return `${value.toFixed(exponent === 0 ? 0 : 1)} ${units[exponent]}`
}
