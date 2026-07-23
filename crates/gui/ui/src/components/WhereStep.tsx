import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { useHomeDirectory, useWhereCheck } from '@/hooks/useWhere'
import { useWizardStore } from '@/stores/useWizardStore'

/**
 * The WHERE wizard step (issue #101): pick the Output location ($HOME or a writable mount/
 * custom path), pre-checked against the same Backup-dir name `backup()` will later compute so
 * a collision is learned here, not mid-run. The chosen location itself is client-only wizard
 * state (`useWizardStore`); the pre-check result is a `core`-derived query (`useWhereCheck`).
 */
export function WhereStep() {
  const home = useHomeDirectory()
  const whereLocation = useWizardStore((state) => state.whereLocation)
  const setWhereLocation = useWizardStore((state) => state.setWhereLocation)
  const [buffer, setBuffer] = useState('')
  const whereCheck = useWhereCheck(whereLocation)

  function choose(location: string) {
    setWhereLocation(location)
  }

  return (
    <div className="flex flex-col gap-4">
      <h2 className="text-lg font-semibold">WHERE</h2>

      <div className="flex gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={!home.data}
          onClick={() => home.data && choose(home.data)}
        >
          Use $HOME
        </Button>
      </div>

      <label className="flex flex-col gap-1 text-sm">
        Custom location
        <div className="flex gap-2">
          <input
            value={buffer}
            onChange={(event) => setBuffer(event.target.value)}
            placeholder="/mnt/backups"
            className="h-8 flex-1 rounded border border-border bg-background px-2"
          />
          <Button size="sm" onClick={() => choose(buffer)} disabled={buffer.length === 0}>
            Use path
          </Button>
        </div>
      </label>

      {whereLocation && whereCheck.isPending && (
        <p className="text-muted-foreground text-sm">Checking {whereLocation}…</p>
      )}

      {whereLocation && whereCheck.isError && (
        <p role="alert" className="text-destructive text-sm">
          {whereCheck.error.message}
        </p>
      )}

      {whereLocation && whereCheck.isSuccess && (
        <p className="text-sm">
          Backup dir will be created at <code>{whereCheck.data.backupDir}</code>
        </p>
      )}
    </div>
  )
}
