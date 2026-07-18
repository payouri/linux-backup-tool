# Backup fails-fast and cleans up; Restore continues on error

**Status:** Accepted · **Date:** 2026-07-17

Backup and Restore use deliberately opposite failure policies. A **backup** run that hits any real read error (permission-denied / I/O / device-gone) aborts the whole run, prints loudly, deletes the freshly-created Backup dir, and exits non-zero — no partial backup is ever left on disk. A **restore** run instead continues past per-file failures and reports `path + reason` at the end rather than aborting.

The principle: **backup governs data at risk** — a half-written archive that looks complete is dangerous, and cleanup is safe because the run created the Backup dir fresh, so fail-fast wins. **Restore is best-effort recovery** — a user putting their environment back wants every file that *can* land to land, so partial success beats all-or-nothing.

## Consequences

The single benign exception on the backup side is a **Vanished** entry (ENOENT after scan — a cache/temp race): skipped-with-note, not fatal. Mount-selection failures are a third, separate policy (recover in-flow, nothing written) and must not be conflated with the backup-run abort.
