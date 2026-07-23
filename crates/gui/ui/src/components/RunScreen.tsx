import { Button } from '@/components/ui/button'
import { useBackupRun } from '@/hooks/useRun'
import { useSelectionSummary } from '@/hooks/useSelectionSummary'
import { useWizardStore } from '@/stores/useWizardStore'
import type { BackupPhase } from '@/types/run'

/** The RUN display label for each backup phase — mirrors `crates/tui`'s `run_screen::
 * phase_label` (`Scanning` shows as "Reading", #61's copy sheet), so both surfaces describe
 * the same run the same way. */
const PHASE_LABEL: Record<BackupPhase, string> = {
  scanning: 'Reading',
  compressing: 'Compressing',
  encrypting: 'Encrypting',
  finalizing: 'Finalizing',
  verifyingArchive: 'Working',
  restoring: 'Working',
}

/** The backup phase display sequence — Encrypting only shown while it's actually reachable
 * (mirrors `crates/tui`'s `display_phases`, #65: a plaintext run never shows it at all). */
const BACKUP_DISPLAY_PHASES: BackupPhase[] = ['scanning', 'compressing', 'encrypting', 'finalizing']

function PhaseRow({ current }: { current: BackupPhase | null }) {
  const currentIndex = current ? BACKUP_DISPLAY_PHASES.indexOf(current) : -1
  return (
    <div className="flex gap-3 text-sm" data-testid="phase-row">
      {BACKUP_DISPLAY_PHASES.map((phase, index) => {
        const state = currentIndex < 0 ? 'pending' : index < currentIndex ? 'done' : index === currentIndex ? 'current' : 'pending'
        return (
          <span
            key={phase}
            data-state={state}
            className={
              state === 'done'
                ? 'text-green-600 dark:text-green-400'
                : state === 'current'
                  ? 'font-semibold text-primary'
                  : 'text-muted-foreground'
            }
          >
            {PHASE_LABEL[phase]}
          </span>
        )
      })}
    </div>
  )
}

/** Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based) — duplicated from
 * `SelectionSummary.tsx` rather than shared, the same call `crates/tui`'s own `format_size`
 * makes ("kept local rather than shared to avoid coupling the screen modules"). */
function formatBytes(bytes: number): string {
  if (bytes === 0) {
    return '0 B'
  }
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const value = bytes / 1024 ** exponent
  return `${value.toFixed(exponent === 0 ? 0 : 1)} ${units[exponent]}`
}

/**
 * The RUN screen (issue #102): the wizard's REVIEW step becomes launching a backup and
 * watching it run. Reflects the WHERE step's chosen location (`useWizardStore`) and the live
 * Selection summary before launch; once started, `useBackupRun` drives everything else —
 * phase/per-Entry progress, the front-half-only cancel affordance, and the terminal outcome
 * (Success/Cancelled/Failed/Internal), matched exhaustively.
 */
export function RunScreen() {
  const whereLocation = useWizardStore((state) => state.whereLocation)
  const summary = useSelectionSummary()
  const run = useBackupRun()

  if (run.status === 'idle') {
    return (
      <div className="flex flex-col gap-4">
        <h2 className="text-lg font-semibold">Review</h2>
        {summary.data && (
          <p className="text-sm">
            {summary.data.entryCount} Entries · {formatBytes(summary.data.totalSizeBytes)} ·{' '}
            {summary.data.encryptionRequired ? 'encrypted' : 'not encrypted'}
          </p>
        )}
        <p className="text-muted-foreground text-sm">
          Backup dir will be created under <code>{whereLocation ?? '—'}</code>
        </p>
        <Button
          size="sm"
          className="self-start"
          disabled={!whereLocation}
          onClick={() => whereLocation && run.start(whereLocation)}
        >
          Start backup
        </Button>
      </div>
    )
  }

  if (run.status === 'running') {
    return (
      <div className="flex flex-col gap-4">
        <h2 className="text-lg font-semibold">Backing up…</h2>
        <PhaseRow current={run.progress.phase} />
        {run.progress.phase === 'scanning' && (
          <p className="text-sm" data-testid="entry-progress">
            {run.progress.entryDone} / {run.progress.entryTotal || summary.data?.entryCount || 0}{' '}
            Entries
          </p>
        )}
        {run.canCancel && (
          <Button variant="outline" size="sm" className="self-start" onClick={run.cancel}>
            Cancel
          </Button>
        )}
      </div>
    )
  }

  if (run.status === 'success') {
    const outcome = run.outcome
    const data = outcome?.type === 'success' ? outcome.data : null
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm font-semibold text-green-600 dark:text-green-400">
          Backup complete
        </p>
        {data && (
          <dl className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">Backup dir</dt>
            <dd data-testid="backup-dir">
              <code>{data.backupDir}</code>
            </dd>
            <dt className="text-muted-foreground">Entries</dt>
            <dd>{data.manifest.entryCount}</dd>
            <dt className="text-muted-foreground">Encryption</dt>
            <dd>{data.manifest.encrypted ? 'encrypted' : 'not encrypted'}</dd>
            {Object.keys(data.manifest.packages).length > 0 && (
              <>
                <dt className="text-muted-foreground">Packages</dt>
                <dd>
                  {Object.entries(data.manifest.packages)
                    .map(([manager, count]) => `${manager} ${count}`)
                    .join(' · ')}
                </dd>
              </>
            )}
            {data.manifest.vanishedCount > 0 && (
              <>
                <dt className="text-muted-foreground">Vanished</dt>
                <dd>{data.manifest.vanishedCount} Entries (recorded in the Manifest)</dd>
              </>
            )}
          </dl>
        )}
        <Button variant="outline" size="sm" className="self-start" onClick={run.reset}>
          Back to review
        </Button>
      </div>
    )
  }

  if (run.status === 'cancelled') {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm font-medium">Backup cancelled</p>
        <p className="text-muted-foreground text-sm">
          Nothing was written — cancelling before the Archive is written leaves nothing behind.
        </p>
        <Button size="sm" className="self-start" onClick={run.reset}>
          Back to review
        </Button>
      </div>
    )
  }

  if (run.status === 'failed') {
    const message = run.outcome?.type === 'failed' ? run.outcome.data.message : ''
    return (
      <div className="flex flex-col gap-3">
        <p role="alert" className="text-destructive text-sm font-medium">
          Backup failed
        </p>
        <p role="alert" className="text-destructive text-sm">
          {message}
        </p>
        <Button variant="outline" size="sm" className="self-start" onClick={run.reset}>
          Back to review
        </Button>
      </div>
    )
  }

  // `internal`: a non-alarming, distinct state (issue #102) — never the raw panic message,
  // which `crates/gui/src/run.rs` deliberately never sends over IPC.
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm font-medium text-amber-600 dark:text-amber-400">
        Something went wrong internally
      </p>
      <p className="text-muted-foreground text-sm">
        Details were logged for diagnosis. You can try again.
      </p>
      <Button size="sm" className="self-start" onClick={run.reset}>
        Back to review
      </Button>
    </div>
  )
}
