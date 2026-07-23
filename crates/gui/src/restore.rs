//! The Restore flow's Rust half (issue #103, spec #97's below-ADR collision clarification):
//! pick a Backup dir, detect encryption, run `core::restore::restore` on the #102 RUN model's
//! `spawn_blocking` worker, and bridge its synchronous `CollisionPolicy::Confirm` callback
//! across IPC as a request→response — the callback sends the full colliding-path list over the
//! same progress [`Channel`] and **blocks the worker thread** on a fresh `std::sync::mpsc`
//! reply channel, whose `Sender` half is stashed in the managed [`CollisionReplyState`] for the
//! separate `resolve_collision` command to deliver into. `core` itself sees nothing of any of
//! this — only the `&mut dyn FnMut(&[PathBuf]) -> bool` its own `CollisionPolicy::Confirm`
//! already contracts for.
//!
//! Reuses issue #102's machinery throughout: `run::ChannelProgress` (generalized here over
//! [`run::ProgressChannelEvent`] so restore's superset channel — Phase/Entry plus
//! `CollisionRequest` — shares the identical throttle rather than a second copy of it),
//! `run::lock_cancel`/[`state::RunCancelState`] (one run in flight at a time, so one cancel
//! slot serves both backup and restore), and `run::log_worker_panic` (host-side-only panic
//! logging, never leaked to the wire).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;

use linux_backup_core::restore::{
    OwnershipOutcome, RestoreFailure, RestoreOutcome, SkippedCollision,
};
use linux_backup_core::{fs_probe, CoreError};

use crate::error::SerializableCoreError;
use crate::run::{lock_cancel, log_worker_panic, PhaseDto, ProgressChannelEvent};
use crate::state::{CollisionReplyState, RunCancelState};

/// The lazy pre-flight probe (stories 39/40): whether `backup_dir` is a real, reachable
/// directory and — only if it is — whether it holds an encrypted Backup. Mirrors
/// `crates/tui`'s `RestoreFlow::new`, which runs the identical two `fs_probe` reads before
/// opening a screen, so the passphrase prompt is shown only when the Backup actually needs
/// one. A plain, infallible read: no managed state, since a Backup-dir pick is a fresh path
/// each call, not something the GUI holds open across commands the way `SelectionModel` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreProbeDto {
    pub exists: bool,
    pub encrypted: bool,
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn restore_probe(backup_dir: String) -> RestoreProbeDto {
    let path = PathBuf::from(&backup_dir);
    let exists = fs_probe::is_dir(&path);
    // Only checked once `exists` holds: an encrypted-Manifest check against a nonexistent dir
    // is meaningless and would always read `false` anyway, so this just skips the stat.
    let encrypted = exists && fs_probe::is_encrypted_backup(&path);
    RestoreProbeDto { exists, encrypted }
}

/// One message sent over `run_restore`'s per-invocation [`tauri::ipc::Channel`]: the same
/// Phase/Entry notifications backup's `ProgressEventDto` carries, plus `CollisionRequest` —
/// the one-shot "here is the full colliding-path list" message the blocked
/// `CollisionPolicy::Confirm` callback sends before it blocks the worker on `resolve_collision`'s
/// reply. Riding the *same* channel as progress (rather than a second Tauri event) keeps "one
/// run, one ordered stream" true for restore exactly as it is for backup.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RestoreEventDto {
    Phase { phase: PhaseDto },
    Entry { done: usize, total: usize },
    CollisionRequest { paths: Vec<String> },
}

impl ProgressChannelEvent for RestoreEventDto {
    fn phase(phase: PhaseDto) -> Self {
        Self::Phase { phase }
    }

    fn entry(done: usize, total: usize) -> Self {
        Self::Entry { done, total }
    }
}

/// The wire-safe mirror of [`OwnershipOutcome`] (story 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OwnershipOutcomeDto {
    Restored,
    SkippedNonRoot,
}

impl From<OwnershipOutcome> for OwnershipOutcomeDto {
    fn from(outcome: OwnershipOutcome) -> Self {
        match outcome {
            OwnershipOutcome::Restored => Self::Restored,
            OwnershipOutcome::SkippedNonRoot => Self::SkippedNonRoot,
        }
    }
}

/// The wire-safe mirror of [`RestoreFailure`] (story 48: per-file failures, listed without
/// aborting the run).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFailureDto {
    pub path: String,
    pub reason: String,
}

impl From<&RestoreFailure> for RestoreFailureDto {
    fn from(failure: &RestoreFailure) -> Self {
        Self {
            path: failure.path.clone(),
            reason: failure.reason.clone(),
        }
    }
}

/// The wire-safe mirror of [`SkippedCollision`] (story 43: a declined collision is a
/// deliberate skip, structurally distinct from [`RestoreFailureDto`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedCollisionDto {
    pub path: String,
}

impl From<&SkippedCollision> for SkippedCollisionDto {
    fn from(skipped: &SkippedCollision) -> Self {
        Self {
            path: skipped.path.clone(),
        }
    }
}

/// The `Success` payload of [`RestoreRunOutcomeDto`]: every fact the report screen needs.
/// `restored_count` is a count, not the full path list — mirroring `crates/tui`'s own report
/// (and backup's `ManifestSummaryDto`) rather than a list that can run to the size of the
/// whole Selection; `failures`/`skipped_collisions` stay structurally apart (ADR-0002 / issue
/// #37's "a declined collision is not a failure") and are small enough (one entry per problem,
/// never per restored file) to ship in full.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcomeDto {
    pub restored_count: usize,
    pub failures: Vec<RestoreFailureDto>,
    pub skipped_collisions: Vec<SkippedCollisionDto>,
    pub ownership: OwnershipOutcomeDto,
    pub reinstall_script_path: String,
    /// Per-manager Package counts (manager name -> count) backing `reinstall.sh` (story 47).
    pub package_counts: BTreeMap<String, usize>,
    /// Set when the run stopped early via its `CancelToken` (ADR-0008 Decision 3's restore
    /// asymmetry): the `Ok`-side partial outcome. Unlike backup, restore has no distinct
    /// `Cancelled` terminal variant — there is no cancel-safe front half, so there is no run
    /// that both cancels and produces nothing to report.
    pub cancelled: bool,
}

impl From<&RestoreOutcome> for RestoreOutcomeDto {
    fn from(outcome: &RestoreOutcome) -> Self {
        Self {
            restored_count: outcome.restored_paths.len(),
            failures: outcome
                .failures
                .iter()
                .map(RestoreFailureDto::from)
                .collect(),
            skipped_collisions: outcome
                .skipped_collisions
                .iter()
                .map(SkippedCollisionDto::from)
                .collect(),
            ownership: OwnershipOutcomeDto::from(outcome.ownership),
            reinstall_script_path: outcome.reinstall_script_path.to_string_lossy().into_owned(),
            package_counts: outcome.package_counts.clone(),
            cancelled: outcome.cancelled,
        }
    }
}

/// The one boundary enum the frontend matches exhaustively for a restore run — the restore
/// analogue of [`crate::run::RunOutcomeDto`]. No separate `Cancelled` variant: restore's
/// cancellation is already `Ok`-side data on [`RestoreOutcomeDto::cancelled`] rather than a
/// `CoreError::Cancelled` to map (ADR-0008 Decision 3's restore asymmetry). A real `CoreError`
/// (an unreadable Backup dir, a wrong passphrase, a checksum mismatch) becomes `Failed`; a
/// worker panic becomes `Internal`, mirroring backup's panic handling exactly.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum RestoreRunOutcomeDto {
    Success(RestoreOutcomeDto),
    Failed(SerializableCoreError),
    Internal(String),
}

/// The fixed, non-alarming `Internal` message for a restore run — restore's own wording
/// (distinct from backup's `INTERNAL_RUN_FAILURE_MESSAGE`) since the two runs are named
/// differently in the UI, even though the underlying panic handling is identical.
const INTERNAL_RESTORE_FAILURE_MESSAGE: &str =
    "The restore stopped unexpectedly due to an internal error. Details were logged for diagnosis.";

/// The pure `join_result -> RestoreRunOutcomeDto` mapping, factored out of `run_restore` so
/// it's unit-testable without a live Tauri app/managed state — the restore analogue of
/// `run::map_join_result`.
fn map_restore_join_result(
    join_result: tauri::Result<Result<RestoreOutcome, CoreError>>,
) -> RestoreRunOutcomeDto {
    match join_result {
        Ok(Ok(outcome)) => RestoreRunOutcomeDto::Success(RestoreOutcomeDto::from(&outcome)),
        Ok(Err(error)) => RestoreRunOutcomeDto::Failed(SerializableCoreError::from(&error)),
        Err(tauri::Error::JoinError(join_error)) => {
            log_worker_panic("restore", join_error);
            RestoreRunOutcomeDto::Internal(INTERNAL_RESTORE_FAILURE_MESSAGE.to_string())
        }
        Err(other) => {
            eprintln!("[linux-backup-gui] restore worker join failed: {other}");
            RestoreRunOutcomeDto::Internal(INTERNAL_RESTORE_FAILURE_MESSAGE.to_string())
        }
    }
}

/// Recovers a poisoned lock the same way `run.rs`'s `lock_cancel` does.
fn lock_collision(
    state: &CollisionReplyState,
) -> std::sync::MutexGuard<'_, Option<mpsc::Sender<bool>>> {
    state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Delivers `overwrite` into whatever collision-confirm reply is currently pending, if any —
/// the logic `resolve_collision` delegates to, kept as a plain function so it's testable
/// without a live `tauri::State`.
///
/// A no-op when nothing is pending: no collision was ever raised this run, the run already
/// resolved it, or — the "window closes mid-prompt" case — nothing is listening for the
/// answer any more. In every case there is simply nothing here to deliver into; the worker
/// thread that *is* still blocked (if any) stays blocked until the whole process exits, since
/// nothing else can supply an answer once its one reply channel's `Sender` is gone. This is an
/// accepted limitation of a synchronous-callback bridge: `core::restore`'s
/// `CollisionPolicy::Confirm` contract has no cancel/timeout path of its own to fall back to.
pub fn resolve_collision_reply(state: &CollisionReplyState, overwrite: bool) {
    if let Some(sender) = lock_collision(state).take() {
        let _ = sender.send(overwrite);
    }
}

/// The *other* half of the request→response bridge (issue #103): a separate Tauri command
/// from `run_restore` (mirroring `cancel_backup`'s separateness from `run_backup`) so the
/// blocking reply travels on its own call, never mixed into the one-way progress `channel`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn resolve_collision(collision_state: tauri::State<'_, CollisionReplyState>, overwrite: bool) {
    resolve_collision_reply(&collision_state, overwrite);
}

/// Front-half-ish cancellation for restore (per-Entry in the unpack loop, ADR-0008 Decision
/// 3): flips the current run's `CancelToken`. Shares [`RunCancelState`]'s single slot with
/// backup's `cancel_backup` — the GUI never runs a backup and a restore at once (one
/// `useMutation` in flight at a time, now enforced backend-side by `run::try_begin_run`), so
/// one slot for "whichever run is current" suffices. A distinct command name (rather than
/// reusing `cancel_backup` itself) keeps the frontend call site naming the run it's actually
/// cancelling.
///
/// **Also** delivers a decline into any pending collision-confirm reply (issue #103 review
/// fix). A worker blocked on the collision prompt is parked in `resolve_overwrite`, *before*
/// `unpack_all`'s `cancel.is_cancelled()` checkpoint — flipping the token alone would leave it
/// stranded forever, since nothing else can ever answer the prompt once it's showing (the
/// `Sender` lives in managed state for the app's whole lifetime, so its `Receiver`'s `recv()`
/// would otherwise never return). Delivering `false` here unblocks it immediately: the worker
/// resumes with `overwrite = false`, then hits the now-flipped cancellation checkpoint on its
/// very next iteration and stops with the partial outcome — Cancel works during the prompt
/// instead of only after it.
/// The logic `cancel_restore` delegates to, kept as a plain function so it's testable
/// without a live `tauri::State` — the restore analogue of [`resolve_collision_reply`].
pub(crate) fn cancel_restore_run(
    cancel_state: &RunCancelState,
    collision_state: &CollisionReplyState,
) {
    if let Some(token) = lock_cancel(cancel_state).as_ref() {
        token.cancel();
    }
    resolve_collision_reply(collision_state, false);
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn cancel_restore(
    cancel_state: tauri::State<'_, RunCancelState>,
    collision_state: tauri::State<'_, CollisionReplyState>,
) {
    cancel_restore_run(&cancel_state, &collision_state);
}

/// The restore RUN command (issue #103): mirrors `run.rs`'s `run_backup` shape — `core::
/// restore::restore` runs on a `spawn_blocking` worker and its return value *is* the terminal
/// result. See `run.rs`'s doc on `run_backup_impl` for why this lives in its own one-function
/// module with a module-scoped `disallowed_macros` waiver (`#[tauri::command]`'s codegen for
/// an `async` fn taking reference-shaped inputs emits a compile-time-only `unreachable!()`
/// sibling item this crate cannot instead express as a `Result`).
pub mod run_restore_impl {
    #![allow(clippy::disallowed_macros)]

    use std::path::PathBuf;
    use std::sync::mpsc;

    use secrecy::SecretString;
    use tauri::ipc::Channel;
    use tauri::State;

    use linux_backup_core::restore::{restore as core_restore, CollisionPolicy};

    use crate::error::SerializableCoreError;
    use crate::run::{try_begin_run, ChannelProgress};
    use crate::state::{CollisionReplyState, RunCancelState, RunContextState};

    use super::{
        lock_cancel, lock_collision, map_restore_join_result, RestoreEventDto, RestoreRunOutcomeDto,
    };

    /// Runs a restore against `backup_dir`, decrypting with `passphrase` when the Backup is
    /// encrypted (`None` is a no-op for a plaintext Backup; a wrong or missing passphrase on an
    /// encrypted one fails as `CoreError::DecryptionFailed` — a typed `Failed` outcome — before
    /// any file is touched, per `core::restore::restore`'s own contract).
    ///
    /// The collision bridge: the worker's `confirm` closure below sends the full
    /// colliding-path list over the *same* `channel` progress rides, then **blocks the worker
    /// thread** on a fresh `std::sync::mpsc` reply channel — its `Sender` half stashed in the
    /// managed [`CollisionReplyState`] — until the separate `resolve_collision` command
    /// delivers the user's single yes/no into it. `core` sees only the
    /// `&mut dyn FnMut(&[PathBuf]) -> bool` its own `CollisionPolicy::Confirm` already
    /// contracts for; it never knows this crosses IPC at all.
    ///
    /// Refuses to start (`Err(SerializableCoreError::already_running)`) if a run — backup or
    /// restore — is already in flight, via [`try_begin_run`] sharing the exact
    /// [`RunCancelState`] slot `run_backup` claims (issue #103 review fix): without this, a
    /// second run overwrote the first run's slot outright, silently dropping its
    /// `CancelToken`/collision `Sender` and corrupting both runs' outcomes.
    #[allow(clippy::needless_pass_by_value)]
    #[tauri::command]
    pub async fn run_restore(
        run_context_state: State<'_, RunContextState>,
        cancel_state: State<'_, RunCancelState>,
        collision_state: State<'_, CollisionReplyState>,
        backup_dir: String,
        passphrase: Option<String>,
        channel: Channel<RestoreEventDto>,
    ) -> Result<RestoreRunOutcomeDto, SerializableCoreError> {
        let cancel_state = cancel_state.inner().clone();

        // Claimed before anything else — a run already in flight is refused here rather than
        // this command doing any further work first (see `try_begin_run`'s doc).
        let Some(cancel) = try_begin_run(&cancel_state) else {
            return Err(SerializableCoreError::already_running());
        };

        let ctx = run_context_state.0.clone();
        let backup_dir = PathBuf::from(backup_dir);
        let passphrase = passphrase.map(SecretString::from);
        let collision_state = collision_state.inner().clone();

        // A second, independent clone of the *same* channel (`Channel::clone` is a cheap
        // `Arc` clone, see `tauri::ipc::Channel`): `confirm` below sends its one-shot
        // `CollisionRequest` over it while `progress` sends every Phase/Entry — one run, one
        // ordered stream on the frontend's side, even though two Rust-side handles exist.
        let collision_channel = channel.clone();

        let join_result = tauri::async_runtime::spawn_blocking(move || {
            let mut progress = ChannelProgress::new(channel);
            let mut confirm = move |paths: &[PathBuf]| -> bool {
                let (reply_tx, reply_rx) = mpsc::channel::<bool>();
                *lock_collision(&collision_state) = Some(reply_tx);
                let dto_paths = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<String>>();
                let _ =
                    collision_channel.send(RestoreEventDto::CollisionRequest { paths: dto_paths });
                // Blocks this worker thread until `resolve_collision` delivers the answer (or
                // the sender is dropped, which — since the slot lives in managed state for the
                // app's whole lifetime — only happens if the app itself is tearing down, at
                // which point this thread is about to be gone anyway; declining is the safer
                // default in that sliver).
                let overwrite = reply_rx.recv().unwrap_or(false);
                *lock_collision(&collision_state) = None;
                overwrite
            };
            let policy = CollisionPolicy::Confirm(&mut confirm);
            core_restore(
                &backup_dir,
                &ctx,
                passphrase.as_ref(),
                policy,
                &mut progress,
                &cancel,
            )
        })
        .await;

        *lock_cancel(&cancel_state) = None;

        Ok(map_restore_join_result(join_result))
    }
}

// Deliberately *not* re-exported as `restore::run_restore` via a plain `pub use` — see
// `run.rs`'s identical note on `run_backup_impl::run_backup`: `#[tauri::command]`'s hidden
// sibling items are resolved by `tauri::generate_handler!` relative to the exact path given,
// so `lib.rs` names `restore::run_restore_impl::run_restore` directly.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::mpsc;

    use linux_backup_core::manifest::{Manifest, SCHEMA_VERSION};
    use linux_backup_core::restore::{
        OwnershipOutcome, RestoreFailure, RestoreOutcome, SkippedCollision,
    };
    use linux_backup_core::CoreError;

    use super::{
        cancel_restore_run, map_restore_join_result, resolve_collision_reply, restore_probe,
        OwnershipOutcomeDto, PhaseDto, RestoreEventDto, RestoreFailureDto, RestoreOutcomeDto,
        RestoreRunOutcomeDto, SkippedCollisionDto, INTERNAL_RESTORE_FAILURE_MESSAGE,
    };
    use crate::error::SerializableCoreError;
    use crate::state::{CollisionReplyState, RunCancelState};
    use linux_backup_core::CancelToken;

    fn empty_manifest() -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            tool_version: "test".to_string(),
            created_at: "1970-01-01T00:00:00Z".to_string(),
            host: "test-host".to_string(),
            user: "test-user".to_string(),
            uid: 1000,
            home: PathBuf::from("/home/test-user"),
            os: "linux-test".to_string(),
            entries: Vec::new(),
            checksums: BTreeMap::new(),
            archive_sha256: "deadbeef".to_string(),
            vanished: Vec::new(),
            packages: BTreeMap::new(),
        }
    }

    fn outcome(cancelled: bool) -> RestoreOutcome {
        RestoreOutcome {
            manifest: empty_manifest(),
            restored_paths: vec![
                PathBuf::from("/home/tester/a"),
                PathBuf::from("/home/tester/b"),
            ],
            failures: vec![RestoreFailure {
                path: "/home/tester/c".to_string(),
                reason: "permission denied".to_string(),
            }],
            skipped_collisions: vec![SkippedCollision {
                path: "/home/tester/d".to_string(),
            }],
            ownership: OwnershipOutcome::SkippedNonRoot,
            reinstall_script_path: PathBuf::from("/home/tester/reinstall.sh"),
            package_counts: BTreeMap::from([("pacman".to_string(), 2)]),
            cancelled,
        }
    }

    #[test]
    fn restore_probe_reports_exists_and_encrypted_for_an_encrypted_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("manifest.json.age"), b"ciphertext").expect("write");

        let probe = restore_probe(tmp.path().to_string_lossy().into_owned());

        assert!(probe.exists);
        assert!(probe.encrypted);
    }

    #[test]
    fn restore_probe_reports_exists_without_encrypted_for_a_plaintext_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("manifest.json"), b"{}").expect("write");

        let probe = restore_probe(tmp.path().to_string_lossy().into_owned());

        assert!(probe.exists);
        assert!(!probe.encrypted);
    }

    #[test]
    fn restore_probe_reports_missing_for_a_nonexistent_dir() {
        let probe = restore_probe("/definitely/not/a/real/path/xyzzy".to_string());

        assert!(!probe.exists);
        assert!(!probe.encrypted);
    }

    #[test]
    fn restore_event_dto_phase_and_entry_wire_shapes() {
        let phase = serde_json::to_value(RestoreEventDto::Phase {
            phase: PhaseDto::VerifyingArchive,
        })
        .expect("serialize");
        assert_eq!(
            phase,
            serde_json::json!({"type": "phase", "phase": "verifyingArchive"})
        );

        let entry =
            serde_json::to_value(RestoreEventDto::Entry { done: 2, total: 5 }).expect("serialize");
        assert_eq!(
            entry,
            serde_json::json!({"type": "entry", "done": 2, "total": 5})
        );
    }

    #[test]
    fn restore_event_dto_collision_request_wire_shape() {
        let event = RestoreEventDto::CollisionRequest {
            paths: vec!["/home/tester/a".to_string(), "/home/tester/b".to_string()],
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "type": "collisionRequest",
                "paths": ["/home/tester/a", "/home/tester/b"],
            })
        );
    }

    #[test]
    fn ownership_outcome_dto_mirrors_each_core_variant() {
        assert_eq!(
            OwnershipOutcomeDto::from(OwnershipOutcome::Restored),
            OwnershipOutcomeDto::Restored
        );
        assert_eq!(
            OwnershipOutcomeDto::from(OwnershipOutcome::SkippedNonRoot),
            OwnershipOutcomeDto::SkippedNonRoot
        );
    }

    #[test]
    fn restore_outcome_dto_reports_counts_failures_skips_ownership_and_reinstall() {
        let dto = RestoreOutcomeDto::from(&outcome(false));

        assert_eq!(dto.restored_count, 2);
        assert_eq!(
            dto.failures,
            vec![RestoreFailureDto {
                path: "/home/tester/c".to_string(),
                reason: "permission denied".to_string(),
            }]
        );
        assert_eq!(
            dto.skipped_collisions,
            vec![SkippedCollisionDto {
                path: "/home/tester/d".to_string(),
            }]
        );
        assert_eq!(dto.ownership, OwnershipOutcomeDto::SkippedNonRoot);
        assert_eq!(dto.reinstall_script_path, "/home/tester/reinstall.sh");
        assert_eq!(dto.package_counts.get("pacman"), Some(&2));
        assert!(!dto.cancelled);
    }

    #[test]
    fn restore_run_outcome_success_wire_shape_carries_cancelled_and_counts() {
        let dto = RestoreRunOutcomeDto::Success(RestoreOutcomeDto::from(&outcome(true)));
        let json = serde_json::to_value(&dto).expect("serialize");

        assert_eq!(json["type"], "success");
        assert_eq!(json["data"]["cancelled"], true);
        assert_eq!(json["data"]["restoredCount"], 2);
        assert_eq!(
            json["data"]["reinstallScriptPath"],
            "/home/tester/reinstall.sh"
        );
    }

    #[test]
    fn restore_run_outcome_failed_wire_shape_carries_the_serializable_core_error() {
        let error =
            SerializableCoreError::from(&CoreError::DecryptionFailed("bad passphrase".to_string()));
        let json = serde_json::to_value(RestoreRunOutcomeDto::Failed(error)).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "type": "failed",
                "data": {"kind": "decryption_failed", "message": "failed to decrypt: bad passphrase"}
            })
        );
    }

    #[test]
    fn restore_run_outcome_internal_wire_shape_carries_only_the_sanitized_message() {
        let json = serde_json::to_value(RestoreRunOutcomeDto::Internal("boom".to_string()))
            .expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "internal", "data": "boom"})
        );
    }

    #[test]
    fn a_real_core_error_maps_to_failed_not_internal() {
        let mapped = map_restore_join_result(Ok(Err(CoreError::ManifestMissing(PathBuf::from(
            "/backups/x",
        )))));
        assert!(matches!(mapped, RestoreRunOutcomeDto::Failed(_)));
    }

    #[test]
    fn a_successful_restore_maps_to_success_with_the_outcome_dto() {
        let mapped = map_restore_join_result(Ok(Ok(outcome(false))));
        match mapped {
            RestoreRunOutcomeDto::Success(dto) => {
                assert_eq!(dto.restored_count, 2);
                assert!(!dto.cancelled);
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_partial_restore_still_maps_to_success_carrying_cancelled_true() {
        // Restore's cancellation is Ok-side data (ADR-0008 D3's asymmetry), never mapped to a
        // distinct terminal variant the way backup's `CoreError::Cancelled` is.
        let mapped = map_restore_join_result(Ok(Ok(outcome(true))));
        match mapped {
            RestoreRunOutcomeDto::Success(dto) => assert!(dto.cancelled),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    /// The required boundary seam (issue #103, mirroring `run.rs`'s identical test): a
    /// *simulated worker panic* — a real `spawn_blocking` task that panics, joined for a real
    /// `JoinError` — must map to `Internal` carrying only the fixed, sanitized message, never
    /// the raw panic payload.
    #[test]
    fn a_simulated_worker_panic_maps_to_internal_without_leaking_the_raw_message() {
        let join_result: tauri::Result<Result<RestoreOutcome, CoreError>> =
            tauri::async_runtime::block_on(async {
                tauri::async_runtime::spawn_blocking(|| -> Result<RestoreOutcome, CoreError> {
                    panic!("boom: simulated restore worker panic")
                })
                .await
            });
        assert!(join_result.is_err(), "a panicking task must join as an Err");

        let mapped = map_restore_join_result(join_result);

        match mapped {
            RestoreRunOutcomeDto::Internal(message) => {
                assert_eq!(message, INTERNAL_RESTORE_FAILURE_MESSAGE);
                assert!(
                    !message.contains("boom"),
                    "the raw panic payload must never cross the IPC boundary, got: {message:?}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// The collision bridge's core mechanism (issue #103): a worker thread blocked on the
    /// reply `Receiver` is unblocked by `resolve_collision_reply` delivering into the matching
    /// `Sender` stashed in the managed state — the exact "one boolean in" half of "one
    /// collision list out, one boolean in, worker blocked between".
    #[test]
    fn resolve_collision_reply_unblocks_a_worker_waiting_on_the_reply_channel() {
        let state = CollisionReplyState::default();
        let (tx, rx) = mpsc::channel::<bool>();
        *state.0.lock().expect("lock collision state") = Some(tx);

        let worker = std::thread::spawn(move || rx.recv().unwrap_or(false));

        resolve_collision_reply(&state, true);

        assert!(
            worker.join().expect("worker thread must not panic"),
            "the delivered answer must be the one the worker receives"
        );
    }

    /// The "window closes mid-prompt" case: nothing pending means nothing to deliver into —
    /// must not panic.
    #[test]
    fn resolve_collision_reply_is_a_no_op_when_nothing_is_pending() {
        let state = CollisionReplyState::default();
        resolve_collision_reply(&state, true);
    }

    /// The review-fix seam (issue #103): `cancel_restore` must unblock a worker parked on the
    /// collision prompt, not just flip the token — the worker there is blocked in
    /// `resolve_overwrite`, *before* `unpack_all`'s `is_cancelled` checkpoint, so a token flip
    /// alone would leave it stranded forever (the `Sender` lives in managed state for the
    /// app's whole lifetime, so its `Receiver` would otherwise never see a value or a
    /// disconnect). `cancel_restore_run` must both flip the token *and* deliver `false`
    /// into the pending reply, so the worker resumes with `overwrite = false`, immediately
    /// observes the now-cancelled token, and — per `core::restore`'s own
    /// `restore_cancelled_mid_unpack_returns_partial_outcome_marked_cancelled` test — stops
    /// with the partial outcome rather than running the collision decision to completion.
    #[test]
    fn cancel_restore_run_flips_the_token_and_unblocks_a_worker_parked_on_the_collision_prompt() {
        let cancel_state = RunCancelState::default();
        let collision_state = CollisionReplyState::default();

        let cancel = CancelToken::new();
        *cancel_state.0.lock().expect("lock cancel state") = Some(cancel.clone());

        let (reply_tx, reply_rx) = mpsc::channel::<bool>();
        *collision_state.0.lock().expect("lock collision state") = Some(reply_tx);

        // Mirrors `run_restore`'s worker: parked in the `confirm` callback exactly where
        // `core::restore::resolve_overwrite` blocks, before any cancellation checkpoint fires.
        let worker = std::thread::spawn(move || reply_rx.recv().unwrap_or(false));

        cancel_restore_run(&cancel_state, &collision_state);

        let overwrite = worker
            .join()
            .expect("the parked worker thread must not panic");
        assert!(
            !overwrite,
            "cancelling during the prompt must decline the collision, never silently overwrite"
        );
        assert!(
            cancel.is_cancelled(),
            "cancel_restore must also flip the run's CancelToken, so the unblocked worker's \
             very next iteration hits the cancellation checkpoint"
        );
    }

    /// A no-op collision slot must not stop `cancel_restore_run` from still flipping the
    /// token — the ordinary (non-collision) cancel path must keep working unchanged.
    #[test]
    fn cancel_restore_run_still_flips_the_token_when_no_collision_is_pending() {
        let cancel_state = RunCancelState::default();
        let collision_state = CollisionReplyState::default();
        let cancel = CancelToken::new();
        *cancel_state.0.lock().expect("lock cancel state") = Some(cancel.clone());

        cancel_restore_run(&cancel_state, &collision_state);

        assert!(cancel.is_cancelled());
    }
}
