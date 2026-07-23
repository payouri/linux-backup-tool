import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { useRestore } from '@/hooks/useRestore'
import { useRestoreProbe } from '@/hooks/useRestoreProbe'
import type { BackupPhase } from '@/types/run'

/**
 * Extracts a human-readable message from a rejected `invoke()` promise (issue #103 review
 * fix): a fallible command's `Err` — e.g. `SerializableCoreError::already_running()`'s
 * `{kind, message}` — arrives here as the mutation's `error`, not wrapped in a JS `Error`
 * instance, so `.message` is read structurally rather than via `instanceof Error`.
 */
function errorMessage(error: unknown): string {
  if (
    error !== null &&
    typeof error === 'object' &&
    'message' in error &&
    typeof (error as { message: unknown }).message === 'string'
  ) {
    return (error as { message: string }).message
  }
  return 'The restore could not be started.'
}

const PHASE_LABEL: Record<BackupPhase, string> = {
  scanning: 'Reading',
  compressing: 'Compressing',
  encrypting: 'Encrypting',
  finalizing: 'Finalizing',
  verifyingArchive: 'Verifying archive',
  restoring: 'Restoring',
}

/**
 * The blocked-worker collision confirmation (issue #103's hard part): shown the moment a
 * `collisionRequest` arrives over the run channel, naming every colliding target at once (per
 * `core::restore`'s "ask once with the full list" contract) with a single yes/no. Resolving
 * either way unblocks the worker thread `run_restore` parked on its reply channel.
 */
function CollisionConfirm({
  paths,
  onResolve,
}: {
  paths: string[]
  onResolve: (overwrite: boolean) => void
}) {
  return (
    <div
      role="dialog"
      aria-label="Overwrite existing files?"
      className="flex flex-col gap-3 rounded-md border border-amber-500 bg-background p-4"
    >
      <p className="text-sm font-medium">
        {paths.length} existing file{paths.length === 1 ? '' : 's'} would be overwritten:
      </p>
      <ul className="max-h-40 overflow-y-auto text-sm">
        {paths.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <div className="flex gap-2">
        <Button variant="outline" size="sm" onClick={() => onResolve(false)}>
          Skip these
        </Button>
        <Button variant="destructive" size="sm" onClick={() => onResolve(true)}>
          Overwrite all
        </Button>
      </div>
    </div>
  )
}

/**
 * The Restore flow (issue #103): pick a Backup dir, decrypt only if needed, run the restore
 * with the collision confirm/progress/cancel bridge `useRestore` drives, and report the
 * best-effort result (restored/skipped-collision/failed, ownership, `reinstall.sh`).
 */
export function RestoreScreen() {
  const [backupDirInput, setBackupDirInput] = useState('')
  const [backupDir, setBackupDir] = useState<string | null>(null)
  const [passphrase, setPassphrase] = useState('')
  const probe = useRestoreProbe(backupDir)
  const restore = useRestore()

  if (restore.status === 'running') {
    return (
      <div className="flex flex-col gap-4">
        <h2 className="text-lg font-semibold">Restoring…</h2>
        <p className="text-sm" data-testid="restore-phase">
          {restore.progress.phase ? PHASE_LABEL[restore.progress.phase] : 'Starting'}
        </p>
        {restore.progress.entryTotal > 0 && (
          <p className="text-sm" data-testid="entry-progress">
            {restore.progress.entryDone} / {restore.progress.entryTotal} Entries
          </p>
        )}
        {restore.awaitingCollisionAnswer && restore.collisionRequest && (
          <CollisionConfirm
            paths={restore.collisionRequest}
            onResolve={restore.resolveCollision}
          />
        )}
        <Button variant="outline" size="sm" className="self-start" onClick={restore.cancel}>
          Cancel
        </Button>
      </div>
    )
  }

  if (restore.status === 'success') {
    const data = restore.outcome?.type === 'success' ? restore.outcome.data : null
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm font-semibold text-green-600 dark:text-green-400">
          {data?.cancelled ? 'Restore cancelled — partial result' : 'Restore complete'}
        </p>
        {data && (
          <dl className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">Restored</dt>
            <dd data-testid="restored-count">{data.restoredCount} Entries</dd>
            <dt className="text-muted-foreground">Ownership</dt>
            <dd>{data.ownership === 'restored' ? 'restored (root)' : 'skipped (non-root)'}</dd>
            <dt className="text-muted-foreground">reinstall.sh</dt>
            <dd>
              <code>{data.reinstallScriptPath}</code>
            </dd>
            {Object.keys(data.packageCounts).length > 0 && (
              <>
                <dt className="text-muted-foreground">Packages</dt>
                <dd>
                  {Object.entries(data.packageCounts)
                    .map(([manager, count]) => `${manager} ${count}`)
                    .join(' · ')}
                </dd>
              </>
            )}
            {data.skippedCollisions.length > 0 && (
              <>
                <dt className="text-muted-foreground">Skipped (declined)</dt>
                <dd data-testid="skipped-collisions">
                  {data.skippedCollisions.length} file
                  {data.skippedCollisions.length === 1 ? '' : 's'}
                </dd>
              </>
            )}
            {data.failures.length > 0 && (
              <>
                <dt className="text-muted-foreground">Failures</dt>
                <dd data-testid="restore-failures">
                  {data.failures.map((failure) => (
                    <div key={failure.path}>
                      • {failure.path} — {failure.reason}
                    </div>
                  ))}
                </dd>
              </>
            )}
          </dl>
        )}
        <Button variant="outline" size="sm" className="self-start" onClick={restore.reset}>
          Restore another backup
        </Button>
      </div>
    )
  }

  if (restore.status === 'failed') {
    const message = restore.outcome?.type === 'failed' ? restore.outcome.data.message : ''
    return (
      <div className="flex flex-col gap-3">
        <p role="alert" className="text-destructive text-sm font-medium">
          Restore failed
        </p>
        <p role="alert" className="text-destructive text-sm">
          {message}
        </p>
        <Button variant="outline" size="sm" className="self-start" onClick={restore.reset}>
          Try again
        </Button>
      </div>
    )
  }

  if (restore.status === 'internal') {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm font-medium text-amber-600 dark:text-amber-400">
          Something went wrong internally
        </p>
        <p className="text-muted-foreground text-sm">
          Details were logged for diagnosis. You can try again.
        </p>
        <Button size="sm" className="self-start" onClick={restore.reset}>
          Try again
        </Button>
      </div>
    )
  }

  // idle: pick a Backup dir, then (only if encrypted) a passphrase.
  return (
    <div className="flex flex-col gap-4">
      <h2 className="text-lg font-semibold">Restore</h2>

      {restore.error !== null && (
        <p role="alert" className="text-destructive text-sm">
          {errorMessage(restore.error)}
        </p>
      )}

      <label className="flex flex-col gap-1 text-sm">
        Backup dir
        <div className="flex gap-2">
          <input
            value={backupDirInput}
            onChange={(event) => setBackupDirInput(event.target.value)}
            placeholder="/home/tester/linux-backup-host-1"
            className="h-8 flex-1 rounded border border-border bg-background px-2"
          />
          <Button
            size="sm"
            onClick={() => setBackupDir(backupDirInput)}
            disabled={backupDirInput.length === 0}
          >
            Load
          </Button>
        </div>
      </label>

      {backupDir && probe.isPending && (
        <p className="text-muted-foreground text-sm">Checking {backupDir}…</p>
      )}

      {backupDir && probe.isSuccess && !probe.data.exists && (
        <p role="alert" className="text-destructive text-sm">
          {backupDir} does not exist or is not a directory.
        </p>
      )}

      {backupDir && probe.isSuccess && probe.data.exists && (
        <>
          {probe.data.encrypted && (
            <label className="flex flex-col gap-1 text-sm">
              Passphrase
              <input
                type="password"
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
                className="h-8 rounded border border-border bg-background px-2"
              />
            </label>
          )}
          <Button
            size="sm"
            className="self-start"
            disabled={probe.data.encrypted && passphrase.length === 0}
            onClick={() => restore.start(backupDir, probe.data.encrypted ? passphrase : undefined)}
          >
            Start restore
          </Button>
        </>
      )}
    </div>
  )
}
