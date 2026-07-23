//! The RUN model (issue #102, ADR-0008 Decision 3): one `async` Tauri command runs
//! `core::backup` on a `spawn_blocking` worker and *is* the terminal result — one run, one
//! `useMutation`. Progress streams over a per-invocation `tauri::ipc::Channel`, strictly
//! one-way (`ChannelProgress`); a front-half-only [`CancelToken`] is flipped by a *separate*
//! command (`cancel_backup`), so observation and control never share a channel. A worker
//! panic is caught at the `spawn_blocking` `.await` (a `JoinError`, not an unwind reaching the
//! webview) and becomes the boundary's `RunOutcomeDto::Internal`, with the raw panic payload
//! logged host-side only — never serialized onto the wire.
//!
//! `core` stays synchronous and untouched by any of this: `run_backup` clones the managed
//! `SelectionModel`/`Encryption`/`RunContext` out of their `Mutex`es *before* the
//! `spawn_blocking` call (the same "extract owned data, then move it onto the worker" shape
//! `crates/tui`'s `run_screen::spawn` uses for its `std::thread` worker), so `core::backup`
//! itself never knows it's being driven from an async command.

use std::time::{Duration, Instant};

use tauri::ipc::Channel;

use linux_backup_core::manifest::Manifest;
use linux_backup_core::progress::{Phase, Progress};
use linux_backup_core::{BackupOutcome, CancelToken, CoreError};

use crate::error::SerializableCoreError;
use crate::state::RunCancelState;

/// The wire-safe mirror of [`Phase`] (issue #102). `camelCase` on the wire, matching every
/// other DTO in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PhaseDto {
    Scanning,
    Compressing,
    Encrypting,
    Finalizing,
    VerifyingArchive,
    Restoring,
}

impl From<Phase> for PhaseDto {
    fn from(phase: Phase) -> Self {
        match phase {
            Phase::Scanning => Self::Scanning,
            Phase::Compressing => Self::Compressing,
            Phase::Encrypting => Self::Encrypting,
            Phase::Finalizing => Self::Finalizing,
            Phase::VerifyingArchive => Self::VerifyingArchive,
            Phase::Restoring => Self::Restoring,
        }
    }
}

/// One message sent over the progress [`Channel`] (issue #102) — notifications only, never a
/// terminal result (that travels as `run_backup`'s return value, per ADR-0008 Decision 3).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProgressEventDto {
    Phase { phase: PhaseDto },
    Entry { done: usize, total: usize },
}

/// How often `on_entry` is allowed to send: at most every [`ENTRY_EMIT_STRIDE`] Entries, or
/// every [`ENTRY_EMIT_INTERVAL`] of wall time, whichever comes first — the throttle/coalesce
/// policy ADR-0008 leaves as a tuning choice rather than a contract. A per-Entry send on a
/// large Selection (tens of thousands of Entries) would flood the IPC bridge with messages
/// the UI can't paint that fast anyway; a fixed stride alone would stall the bar on a slow
/// single large file, so both a count and a time bound gate the send. `on_phase` is never
/// throttled — phase changes are coarse (four boundaries per backup) and each one is a fact
/// the RUN screen must not miss.
const ENTRY_EMIT_STRIDE: usize = 25;
const ENTRY_EMIT_INTERVAL: Duration = Duration::from_millis(50);

/// Implemented by every wire-safe payload a [`ChannelProgress`] can send: the two
/// notifications `core::Progress` ever emits, translated into that payload's own shape.
/// Generalizing `ChannelProgress` over this trait — rather than duplicating its throttle
/// logic per screen — is what lets issue #103's restore channel (a superset that also
/// carries a one-shot collision request `ChannelProgress` itself knows nothing about) reuse
/// this exact machinery instead of re-implementing the stride/interval throttle.
pub(crate) trait ProgressChannelEvent: serde::Serialize + Send + 'static {
    fn phase(phase: PhaseDto) -> Self;
    fn entry(done: usize, total: usize) -> Self;
}

impl ProgressChannelEvent for ProgressEventDto {
    fn phase(phase: PhaseDto) -> Self {
        Self::Phase { phase }
    }

    fn entry(done: usize, total: usize) -> Self {
        Self::Entry { done, total }
    }
}

/// The [`Progress`] implementation a run's worker drives: every `core` callback becomes a
/// `T` sent over the channel. Sends are best-effort (`let _ =`) — if the webview already
/// tore down, `core` neither blocks nor observes an error, matching the trait's one-way
/// contract (mirrors `crates/tui`'s `ChannelProgress`). Generic over `T` (issue #103) so
/// `run_restore`'s superset channel reuses the identical throttle rather than a second copy
/// of it.
pub(crate) struct ChannelProgress<T: ProgressChannelEvent> {
    channel: Channel<T>,
    last_entry_emit: Instant,
    last_entry_done: usize,
}

impl<T: ProgressChannelEvent> ChannelProgress<T> {
    pub(crate) fn new(channel: Channel<T>) -> Self {
        Self {
            channel,
            last_entry_emit: Instant::now(),
            last_entry_done: 0,
        }
    }
}

impl<T: ProgressChannelEvent> Progress for ChannelProgress<T> {
    fn on_phase(&mut self, phase: Phase) {
        let _ = self.channel.send(T::phase(phase.into()));
        // A fresh phase resets the throttle window so its own first Entry (if any) isn't
        // starved by the previous phase's last send.
        self.last_entry_emit = Instant::now();
        self.last_entry_done = 0;
    }

    fn on_entry(&mut self, done: usize, total: usize) {
        let final_entry = total > 0 && done >= total;
        let strided = done.saturating_sub(self.last_entry_done) >= ENTRY_EMIT_STRIDE;
        let timed_out = self.last_entry_emit.elapsed() >= ENTRY_EMIT_INTERVAL;
        if !(final_entry || strided || timed_out) {
            return;
        }
        let _ = self.channel.send(T::entry(done, total));
        self.last_entry_emit = Instant::now();
        self.last_entry_done = done;
    }
}

/// The Manifest summary half of a successful [`RunOutcomeDto`] (issue #102's "Success shows
/// the Backup-dir location + a Manifest summary"): counts, not the whole Manifest — the raw
/// Manifest is a Backup-dir-sized artifact the RUN screen never needs to render in full.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSummaryDto {
    pub entry_count: usize,
    pub encrypted: bool,
    /// Per-manager Package counts (manager name -> count), mirroring the RUN screen's
    /// `crates/tui` counterpart (`run_screen::success_lines`'s "pacman 1 · flatpak 2" line).
    pub packages: std::collections::BTreeMap<String, usize>,
    pub vanished_count: usize,
}

fn manifest_summary_dto(manifest: &Manifest, encrypted: bool) -> ManifestSummaryDto {
    ManifestSummaryDto {
        entry_count: manifest.entries.len(),
        encrypted,
        packages: manifest
            .packages
            .iter()
            .map(|(manager, packages)| (manager.clone(), packages.len()))
            .collect(),
        vanished_count: manifest.vanished.len(),
    }
}

/// The `Success` half of [`RunOutcomeDto`]: where the Backup dir landed plus the Manifest
/// summary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOutcomeDto {
    pub backup_dir: String,
    pub manifest: ManifestSummaryDto,
}

fn backup_outcome_dto(outcome: &BackupOutcome, encrypted: bool) -> BackupOutcomeDto {
    BackupOutcomeDto {
        backup_dir: outcome.backup_dir.to_string_lossy().into_owned(),
        manifest: manifest_summary_dto(&outcome.manifest, encrypted),
    }
}

/// The one boundary enum the frontend matches exhaustively (ADR-0008 Decision 3): a real
/// `CoreError` becomes `Failed`; `CoreError::Cancelled` and a worker panic each get their own
/// distinct variant so neither is mistaken for the other — a cancellation is a deliberate,
/// neutral outcome, a panic is an unexpected internal one, and conflating them would either
/// alarm the user over a clean cancel or reassure them over a real bug.
///
/// `#[serde(tag = "type", content = "data")]` (adjacently tagged) rather than internally
/// tagged: `Internal`'s payload is a bare `String`, which internally-tagged serde cannot embed
/// alongside a tag key (it requires a map-shaped payload) — adjacent tagging is the one scheme
/// that serializes every variant here uniformly. See this module's tests for the exact wire
/// shape asserted.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum RunOutcomeDto {
    Success(BackupOutcomeDto),
    Cancelled,
    Failed(SerializableCoreError),
    /// A worker panic (`JoinError`) or any other runtime-level failure this module cannot
    /// attribute to a real `CoreError` — never the raw panic message (that is logged
    /// host-side only, see [`log_worker_panic`]).
    Internal(String),
}

/// The fixed, non-alarming `Internal` message the frontend ever sees (issue #102: "an
/// unexpected internal crash... surfaced as a distinct, non-alarming... state, with details
/// logged for diagnosis"). Never interpolates the panic payload — that would leak raw
/// internals to the one surface (the webview) ADR-0008 says must never see them.
const INTERNAL_RUN_FAILURE_MESSAGE: &str =
    "The backup stopped unexpectedly due to an internal error. Details were logged for diagnosis.";

/// Logs a worker panic host-side (stderr — captured by the launching terminal, or by
/// `journald`/Flatpak's log sink in a packaged run). The raw panic payload (a `&str`, a
/// `String`, or neither) is extracted here rather than left to the process-wide panic hook
/// alone, so the *specific* run that failed is attributed in the log line, not just "a panic
/// happened somewhere."
///
/// `op` names the run kind in the log line (`"backup"`/`"restore"`, issue #103) — the sanitized
/// message each call site sends over IPC is its own constant (backup's
/// [`INTERNAL_RUN_FAILURE_MESSAGE`], restore's own in `restore.rs`), never derived from `op` or
/// the panic payload, so this function's job stops at logging and never touches the wire.
pub(crate) fn log_worker_panic(op: &str, join_error: tokio::task::JoinError) {
    if join_error.is_panic() {
        let payload = join_error.into_panic();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        eprintln!("[linux-backup-gui] {op} worker panicked: {message}");
    } else {
        // Not a panic (e.g. the runtime shut down mid-task) — still an `Internal`-shaped
        // outcome, but there is no panic payload to extract or log beyond the `JoinError`
        // itself.
        eprintln!("[linux-backup-gui] {op} worker join failed without a panic: {join_error}");
    }
}

/// Installs a process-wide panic hook that logs host-side before delegating to the previous
/// (default) hook — the ADR-0008 Decision 3 "host-side panic hook keeps `Internal` panics
/// diagnosable" requirement. This is a *second*, independent line of diagnosis alongside
/// [`log_worker_panic`]'s per-run log line: the hook fires wherever a panic happens (including
/// one this crate's `spawn_blocking` catch site can't reach, e.g. a panic on a different
/// worker), the per-run log line names *which* run failed. Installed once at startup
/// (`crate::run`), matching `crates/tui`'s single-install panic hook (ADR-0005 Decision 7).
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[linux-backup-gui] panic: {info}");
        default_hook(info);
    }));
}

/// Recovers a poisoned lock the same way `commands.rs`'s lock helpers do. `pub(crate)` so
/// issue #103's `restore.rs` can share `RunCancelState`'s single slot for `cancel_restore`
/// rather than duplicating this helper.
pub(crate) fn lock_cancel(
    state: &RunCancelState,
) -> std::sync::MutexGuard<'_, Option<CancelToken>> {
    state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Atomically claims [`RunCancelState`]'s single slot for a fresh run, or refuses if one is
/// already in flight — enforcing on the *backend* the "one run at a time" invariant ADR-0008's
/// run model previously only assumed the frontend would uphold. `run_backup` and `run_restore`
/// share this exact slot (both are managed as the one `RunCancelState` instance), so this one
/// guard also rejects a backup and a restore racing each other, not just two of the same kind.
///
/// The check-and-set happens under a single lock acquisition, so two concurrent callers can't
/// both observe `None` and each believe they own the slot — one wins, the other is refused
/// before it clones anything out of the other managed states or touches `core` at all. Without
/// this, a second run silently overwrote the slot: the first run's `CancelToken` handle for
/// `cancel_backup`/`cancel_restore` to find was gone (misrouting Cancel), and — for a restore
/// blocked on a collision prompt — its [`state::CollisionReplyState`] `Sender` was dropped
/// too, auto-resolving its prompt to a silent decline the user never answered.
///
/// [`state::CollisionReplyState`]: crate::state::CollisionReplyState
pub(crate) fn try_begin_run(state: &RunCancelState) -> Option<CancelToken> {
    let mut guard = lock_cancel(state);
    if guard.is_some() {
        return None;
    }
    let cancel = CancelToken::new();
    *guard = Some(cancel.clone());
    drop(guard);
    Some(cancel)
}

/// The RUN command (issue #102, ADR-0008 Decision 3): runs `core::backup` on a
/// `spawn_blocking` worker and *is* the terminal result. `location` is the WHERE step's chosen
/// Output location (issue #101's `where_check` already validated it; this call recomputes
/// nothing about that choice, it just runs against it) — the managed `RunContextState` was
/// seeded with `output_location: home` at startup (see that module's doc) precisely because no
/// WHERE step existed yet; this is the first command that overrides it per-run rather than
/// using the managed default.
///
/// `Result`-wrapped for two reasons now: Tauri requires it of any `async` command taking
/// reference-shaped inputs (every `State<'_, _>` parameter here), and — since the review that
/// added [`try_begin_run`] — `Err` is a genuine, reachable outcome: a second run started while
/// one is already in flight is refused with [`SerializableCoreError::already_running`] before
/// this command touches `core` at all, rather than silently clobbering the first run's managed
/// state. Every *other* failure mode (a real `CoreError`, a cooperative cancel, a worker panic)
/// still rides the `Ok` payload's [`RunOutcomeDto`], unchanged.
///
/// `disallowed_macros` is waived for [`run_backup_impl`]'s whole one-function module rather
/// than on the function itself: `#[tauri::command]`'s expansion of an `async` fn taking
/// reference-shaped inputs also emits a *sibling* compile-time-only item — `if false { ...
/// unreachable!() ... }` (tauri-macros' `wrapper.rs`, statically checking "an async command
/// taking a reference must return a `Result`") — that never executes but doesn't inherit an
/// `#[allow]` attached only to the annotated function. The panic lives in a dependency's
/// codegen, not in logic this crate could instead express as a `Result`; scoping the waiver to
/// this one-function module (rather than the whole file) keeps it from also silencing a real
/// `unreachable!()`/`panic!()` anywhere else in `run.rs`.
pub mod run_backup_impl {
    #![allow(clippy::disallowed_macros)]

    use tauri::ipc::Channel;
    use tauri::State;

    use linux_backup_core::archive::Encryption;
    use linux_backup_core::{backup, packages::SystemCommandRunner};

    use crate::error::SerializableCoreError;
    use crate::state::{EncryptionState, RunCancelState, RunContextState, SelectionState};

    use super::{
        lock_cancel, map_join_result, try_begin_run, ChannelProgress, ProgressEventDto,
        RunOutcomeDto,
    };

    #[allow(clippy::needless_pass_by_value)]
    #[tauri::command]
    pub async fn run_backup(
        selection_state: State<'_, SelectionState>,
        encryption_state: State<'_, EncryptionState>,
        run_context_state: State<'_, RunContextState>,
        cancel_state: State<'_, RunCancelState>,
        location: String,
        channel: Channel<ProgressEventDto>,
    ) -> Result<RunOutcomeDto, SerializableCoreError> {
        // An owned, `'static` clone of the shared `Arc` (see `RunCancelState`'s doc) — this,
        // not the `State` wrapper itself, is what gets held across the `.await` below.
        let cancel_state = cancel_state.inner().clone();

        // Claimed *before* anything else is cloned out of the other managed states (issue
        // #103 review fix): a run already in flight is refused here, immediately, rather than
        // this command doing a bunch of work first only to clobber the other run's slot at the
        // last moment.
        let Some(cancel) = try_begin_run(&cancel_state) else {
            return Err(SerializableCoreError::already_running());
        };

        // Cloned/pulled out of the managed state (mirrors `crates/tui`'s `BackupRequest`): the
        // worker closure below must own everything it touches, since it runs on a different
        // thread than the one this `async fn`'s state locks are valid on. Every `State<'_, _>`
        // parameter is used only in this section, before the `.await` below — none of their
        // non-`'static` lifetimes needs to survive being held across it.
        let selection = selection_state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let encryption: Encryption = encryption_state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .encryption()
            .clone();
        let mut ctx = run_context_state.0.clone();
        ctx.output_location = std::path::PathBuf::from(location);
        let encrypted = encryption.is_encrypted();

        let join_result = tauri::async_runtime::spawn_blocking(move || {
            let mut progress = ChannelProgress::new(channel);
            let runner = SystemCommandRunner;
            backup(
                &selection,
                &ctx,
                &encryption,
                &runner,
                &mut progress,
                &cancel,
            )
        })
        .await;

        // The run has ended one way or another — clear the slot so a stray `cancel_backup`
        // call after this point (a race the frontend's affordance-hiding should already
        // prevent, but the backend must not rely on that alone) finds nothing to flip.
        *lock_cancel(&cancel_state) = None;

        Ok(map_join_result(join_result, encrypted))
    }
}

// Deliberately *not* re-exported as `run::run_backup` via a plain `pub use`: `#[tauri::command]`
// also generates hidden sibling items (`__cmd__run_backup`, ...) in whatever module directly
// contains the annotated fn, and `tauri::generate_handler!` resolves those relative to the exact
// path given to it — a `pub use` re-export only carries the fn itself, not its hidden siblings.
// `lib.rs`'s `generate_handler!` call therefore names the fully qualified
// `run::run_backup_impl::run_backup` path directly.

/// The pure `join_result -> RunOutcomeDto` mapping (issue #102, ADR-0008 Decision 3's
/// terminal-outcome rule), factored out of [`run_backup`] so it's unit-testable without a live
/// Tauri app/managed state: a real `CoreError` -> `Failed`; `CoreError::Cancelled` -> the
/// distinct neutral `Cancelled`; a worker panic (`JoinError`) -> `Internal`, sanitized via
/// [`log_worker_panic`].
fn map_join_result(
    join_result: tauri::Result<Result<BackupOutcome, CoreError>>,
    encrypted: bool,
) -> RunOutcomeDto {
    match join_result {
        Ok(Ok(outcome)) => RunOutcomeDto::Success(backup_outcome_dto(&outcome, encrypted)),
        // Cancellation is neutral, never a failure (ADR-0008 Decision 3) — rendered as its own
        // variant rather than folded into `Failed`.
        Ok(Err(CoreError::Cancelled)) => RunOutcomeDto::Cancelled,
        Ok(Err(error)) => RunOutcomeDto::Failed(SerializableCoreError::from(&error)),
        Err(tauri::Error::JoinError(join_error)) => {
            log_worker_panic("backup", join_error);
            RunOutcomeDto::Internal(INTERNAL_RUN_FAILURE_MESSAGE.to_string())
        }
        // Any other `tauri::Error` `spawn_blocking`'s `JoinHandle::poll` could in principle
        // surface is just as undiagnosable to the frontend as a panic — treat it the same way
        // rather than inventing a fourth failure shape.
        Err(other) => {
            eprintln!("[linux-backup-gui] backup worker join failed: {other}");
            RunOutcomeDto::Internal(INTERNAL_RUN_FAILURE_MESSAGE.to_string())
        }
    }
}

/// Front-half-only cancellation (issue #102, ADR-0008 Decision 3): flips the current run's
/// [`CancelToken`], a *separate* command from `run_backup` so `Progress` stays strictly
/// one-way (observation and control never share a channel). A no-op if no run is in flight —
/// the frontend hides the cancel affordance once `Finalizing` arrives (this module's doc,
/// `"WritingArchive"` in ADR-0008's prose), but this command doesn't rely on that alone: calling
/// it after a run has already ended, or when none was ever started, simply finds `None` here.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn cancel_backup(cancel_state: tauri::State<'_, RunCancelState>) {
    if let Some(token) = lock_cancel(&cancel_state).as_ref() {
        token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use linux_backup_core::manifest::{Manifest, PackageInfo, VanishedEntry, SCHEMA_VERSION};

    use super::{
        backup_outcome_dto, lock_cancel, manifest_summary_dto, map_join_result, try_begin_run,
        PhaseDto, ProgressEventDto, RunOutcomeDto, INTERNAL_RUN_FAILURE_MESSAGE,
    };
    use crate::error::SerializableCoreError;
    use crate::state::RunCancelState;

    fn manifest_with(
        entries: usize,
        vanished: Vec<VanishedEntry>,
        packages: BTreeMap<String, Vec<PackageInfo>>,
    ) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            tool_version: "test".to_string(),
            created_at: "1970-01-01T00:00:00Z".to_string(),
            host: "thinkpad".to_string(),
            user: "tester".to_string(),
            uid: 1000,
            home: PathBuf::from("/home/tester"),
            os: "test-os".to_string(),
            entries: Vec::new(),
            checksums: BTreeMap::new(),
            archive_sha256: "deadbeef".to_string(),
            vanished,
            packages,
        }
        .tap_entries(entries)
    }

    // Small local helper so `manifest_with` can fill `entries` without a full `Entry` fixture
    // (this module never inspects an Entry's own fields, only the Manifest's own `entries`
    // Vec's length) — kept here rather than imported from `dto.rs`'s test module to avoid a
    // cross-module `#[cfg(test)]` dependency for one field.
    trait TapEntries {
        fn tap_entries(self, count: usize) -> Self;
    }
    impl TapEntries for Manifest {
        fn tap_entries(mut self, count: usize) -> Self {
            self.entries = (0..count)
                .map(|i| linux_backup_core::manifest::ManifestEntry {
                    path: format!("f{i}"),
                    root: linux_backup_core::manifest::ManifestRoot::Home,
                    entry_type: linux_backup_core::manifest::ManifestEntryType::File,
                    category: "Shell".to_string(),
                    sensitive: false,
                })
                .collect();
            self
        }
    }

    #[test]
    fn phase_dto_serializes_camel_case() {
        let json = serde_json::to_string(&PhaseDto::VerifyingArchive).expect("serialize");
        assert_eq!(json, "\"verifyingArchive\"");
    }

    #[test]
    fn progress_event_dto_phase_wire_shape() {
        let event = ProgressEventDto::Phase {
            phase: PhaseDto::Scanning,
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "phase", "phase": "scanning"})
        );
    }

    #[test]
    fn progress_event_dto_entry_wire_shape() {
        let event = ProgressEventDto::Entry { done: 3, total: 10 };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "entry", "done": 3, "total": 10})
        );
    }

    #[test]
    fn manifest_summary_dto_counts_packages_and_vanished() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "pacman".to_string(),
            vec![PackageInfo {
                name: "ripgrep".to_string(),
                version: "14".to_string(),
                origin: None,
                channel: None,
            }],
        );
        let manifest = manifest_with(
            3,
            vec![VanishedEntry {
                path: "home/.cache/x".to_string(),
                sensitive: false,
            }],
            packages,
        );

        let dto = manifest_summary_dto(&manifest, true);

        assert_eq!(dto.entry_count, 3);
        assert!(dto.encrypted);
        assert_eq!(dto.packages.get("pacman"), Some(&1));
        assert_eq!(dto.vanished_count, 1);
    }

    #[test]
    fn run_outcome_success_wire_shape() {
        let outcome = linux_backup_core::BackupOutcome {
            backup_dir: PathBuf::from("/home/tester/linux-backup-host-1"),
            manifest: manifest_with(2, Vec::new(), BTreeMap::new()),
        };
        let dto = RunOutcomeDto::Success(backup_outcome_dto(&outcome, false));

        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "type": "success",
                "data": {
                    "backupDir": "/home/tester/linux-backup-host-1",
                    "manifest": {
                        "entryCount": 2,
                        "encrypted": false,
                        "packages": {},
                        "vanishedCount": 0,
                    }
                }
            })
        );
    }

    #[test]
    fn run_outcome_cancelled_wire_shape_carries_no_data() {
        let json = serde_json::to_value(&RunOutcomeDto::Cancelled).expect("serialize");
        assert_eq!(json, serde_json::json!({"type": "cancelled"}));
    }

    #[test]
    fn run_outcome_failed_wire_shape_carries_the_serializable_core_error() {
        let error = SerializableCoreError::from(&linux_backup_core::CoreError::BlankPassphrase);
        let json = serde_json::to_value(RunOutcomeDto::Failed(error)).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "type": "failed",
                "data": {"kind": "blank_passphrase", "message": "passphrase must not be blank"}
            })
        );
    }

    #[test]
    fn run_outcome_internal_wire_shape_carries_only_the_sanitized_message() {
        let json =
            serde_json::to_value(RunOutcomeDto::Internal("boom".to_string())).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "internal", "data": "boom"})
        );
    }

    #[test]
    fn a_cancelled_core_error_maps_to_the_cancelled_variant_not_failed() {
        let mapped = map_join_result(Ok(Err(linux_backup_core::CoreError::Cancelled)), false);
        assert_eq!(mapped, RunOutcomeDto::Cancelled);
    }

    #[test]
    fn a_real_core_error_maps_to_failed() {
        let error = linux_backup_core::CoreError::BlankPassphrase;
        let mapped = map_join_result(Ok(Err(error)), false);
        assert_eq!(
            mapped,
            RunOutcomeDto::Failed(SerializableCoreError::from(
                &linux_backup_core::CoreError::BlankPassphrase
            ))
        );
    }

    #[test]
    fn a_successful_run_maps_to_success_with_the_manifest_summary() {
        let outcome = linux_backup_core::BackupOutcome {
            backup_dir: PathBuf::from("/home/tester/linux-backup-host-1"),
            manifest: manifest_with(4, Vec::new(), BTreeMap::new()),
        };
        let mapped = map_join_result(Ok(Ok(outcome)), true);
        match mapped {
            RunOutcomeDto::Success(dto) => {
                assert_eq!(dto.backup_dir, "/home/tester/linux-backup-host-1");
                assert_eq!(dto.manifest.entry_count, 4);
                assert!(dto.manifest.encrypted);
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    /// The required boundary seam (issue #102): a *simulated worker panic* — a real
    /// `spawn_blocking` task that panics, joined for a real `JoinError` — must map to
    /// `Internal` carrying only the fixed, sanitized message, never the raw panic payload
    /// (`"boom: simulated worker panic"` here must not appear on the wire).
    #[test]
    fn a_simulated_worker_panic_maps_to_internal_without_leaking_the_raw_message() {
        let join_result: tauri::Result<
            Result<linux_backup_core::BackupOutcome, linux_backup_core::CoreError>,
        > = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(
                || -> Result<linux_backup_core::BackupOutcome, linux_backup_core::CoreError> {
                    panic!("boom: simulated worker panic")
                },
            )
            .await
        });
        assert!(join_result.is_err(), "a panicking task must join as an Err");

        let mapped = map_join_result(join_result, false);

        match mapped {
            RunOutcomeDto::Internal(message) => {
                assert_eq!(message, INTERNAL_RUN_FAILURE_MESSAGE);
                assert!(
                    !message.contains("boom"),
                    "the raw panic payload must never cross the IPC boundary, got: {message:?}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// The one-run-at-a-time enforcement (issue #103 review fix): a second caller must be
    /// refused the slot, never silently overwrite the first run's `CancelToken` — the
    /// vulnerability the review's trace demonstrated (a clobbered slot misroutes `cancel_*`
    /// and, for a restore blocked on a collision prompt, auto-resolves it without the user's
    /// answer). `run_backup`/`run_restore` share this exact `RunCancelState` slot, so this one
    /// guard covers both a same-kind race and a backup-vs-restore race identically.
    #[test]
    fn try_begin_run_refuses_a_second_run_while_one_is_in_flight_without_clobbering_the_first() {
        let state = RunCancelState::default();

        let first = try_begin_run(&state).expect("the first caller must claim the slot");
        let second = try_begin_run(&state);

        assert!(
            second.is_none(),
            "a second run must be refused while one is already in flight"
        );

        // The slot must still reference *this exact* first-run token — proving the refused
        // second attempt never touched it. Cancelling `first` and observing the slot's own
        // token reflect that is the strongest evidence available short of pointer identity.
        first.cancel();
        assert!(
            lock_cancel(&state)
                .as_ref()
                .expect("the slot must still hold the first run's token")
                .is_cancelled(),
            "the slot's token must be the same handle as `first`, unclobbered by the refusal"
        );
    }

    #[test]
    fn try_begin_run_allows_a_fresh_run_once_the_slot_is_cleared() {
        let state = RunCancelState::default();

        let first = try_begin_run(&state).expect("first run claims the slot");
        *lock_cancel(&state) = None; // the run ended, mirroring `run_backup`'s own cleanup.

        let second = try_begin_run(&state);
        assert!(
            second.is_some(),
            "a fresh run must be allowed once the previous one has cleared the slot"
        );
        drop(first);
    }
}
