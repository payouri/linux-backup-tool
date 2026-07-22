//! The restore flow: overwrite-confirm → (passphrase-unlock) → RUN → result (#72, spec #62).
//!
//! Restore is seeded by `linux-backup restore <dir>` (via [`crate::app::App::new_restore`]) and
//! is *not* part of the backup wizard — it is its own route with its own screens, held here in a
//! self-contained [`RestoreFlow`] so the reducer and render stay testable the same way the
//! wizard's do: [`RestoreFlow::handle_key`] and [`RestoreFlow::tick`] are pure folds over
//! hand-built inputs, and [`render`] is assertable through a `TestBackend`.
//!
//! Three spec-locked shapes drive the design:
//!
//! - **Overwrite-confirm** is the single destructive consent (restore is the one destructive
//!   op), so `[ Cancel ]` is focused by default and the copy states what will be written and
//!   that restore is best-effort (ADR-0002). Confirming here *is* the whole-run overwrite
//!   decision, so the worker runs with [`CollisionPolicy::Overwrite`] rather than routing a
//!   per-collision callback across the worker thread — the user already consented up front.
//!   This is the one screen the #61 copy sheet allows to say "existing files".
//! - **Passphrase-unlock** (only for an encrypted Backup, detected by
//!   [`fs_probe::is_encrypted_backup`]) allows **unlimited** retries with a wrong-passphrase
//!   indicator: a wrong passphrase fails `core`'s decryption before a single file is touched
//!   (see [`restore::restore`]), so re-prompting is always safe. The passphrase lives only in
//!   a [`SecretString`]; `F2` reveals it for the current frame alone (never cloned to a
//!   retained `String`, never persisted — story 41 / #51).
//! - **Best-effort partial report** (ADR-0002): success, or the failed Entries as
//!   `• <path> — <os error>`, scrolled and truncated with `… (+N more)` when they overflow.
//!
//! Like the RUN screen (ADR-0005 D4), the restore runs on a `std::thread` worker and its
//! [`Progress`] is bridged over a `std::sync::mpsc` channel — no async, no `tokio`. The launch
//! transition is the one impure step in the reducer (it spawns the worker); every other
//! transition, including folding a worker message back into a screen ([`RestoreFlow::on_msg`]),
//! is pure and unit-tested directly.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::progress::{Phase, Progress};
use linux_backup_core::restore::{self, CollisionPolicy, RestoreFailure, RestoreOutcome};
use linux_backup_core::{fs_probe, CoreError, RunContext};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use secrecy::{ExposeSecret, SecretString};

use crate::reflow::ReflowTier;

/// A passphrase being typed: the canonical store is a [`SecretString`], never a retained plain
/// `String` (story 41 / #51). Editing rebuilds the `SecretString` from a transient owned
/// `String` — an unavoidable step for an editable field — but nothing outside this type ever
/// holds the plaintext, and the value is zeroized when the `SecretString` is dropped.
#[derive(Debug)]
struct SecretInput {
    secret: SecretString,
}

impl SecretInput {
    fn new() -> Self {
        Self {
            secret: SecretString::from(String::new()),
        }
    }

    fn push(&mut self, ch: char) {
        let mut buffer = self.secret.expose_secret().to_owned();
        buffer.push(ch);
        self.secret = SecretString::from(buffer);
    }

    fn backspace(&mut self) {
        let mut buffer = self.secret.expose_secret().to_owned();
        buffer.pop();
        self.secret = SecretString::from(buffer);
    }

    fn char_count(&self) -> usize {
        self.secret.expose_secret().chars().count()
    }

    fn is_empty(&self) -> bool {
        self.secret.expose_secret().is_empty()
    }

    /// The plaintext, exposed for the current render frame only (the `F2` reveal). Never stored
    /// by the caller — it formats the returned slice into cells and drops it.
    fn exposed(&self) -> &str {
        self.secret.expose_secret()
    }

    /// Consumes the field, yielding the passphrase to hand to the worker. Consuming (rather
    /// than cloning) keeps a single owner of the secret up to the point it crosses into `core`.
    fn into_secret(self) -> SecretString {
        self.secret
    }
}

/// The passphrase-unlock screen's mutable state.
#[derive(Debug)]
struct Unlock {
    input: SecretInput,
    /// View-only reveal toggle (`F2`); the passphrase itself stays a [`SecretString`].
    reveal: bool,
    /// Whether the previous attempt failed decryption — drives the wrong-passphrase indicator.
    /// Cleared as soon as the user resumes typing, so the warning tracks the current attempt.
    wrong: bool,
    /// 1-based number of the attempt now being typed. Feeds the `attempt N` indicator (#61) and
    /// increments on each retry — the visible proof that retries are uncapped (#72).
    attempt: usize,
}

impl Unlock {
    fn new() -> Self {
        Self {
            input: SecretInput::new(),
            reveal: false,
            wrong: false,
            attempt: 1,
        }
    }

    /// A retry after a wrong passphrase: a fresh field with the indicator lit, numbered for the
    /// `attempt N` copy. Unlimited retries (#72) are exactly this — a failed decryption returns
    /// here rather than aborting the run, and nothing caps `attempt`.
    fn retry(attempt: usize) -> Self {
        Self {
            input: SecretInput::new(),
            reveal: false,
            wrong: true,
            attempt,
        }
    }
}

/// Live state of the restore worker, polled while [`Stage::Running`] is active.
#[derive(Debug)]
struct Running {
    rx: Receiver<Msg>,
    phase: Option<Phase>,
    done: usize,
    total: usize,
    /// The 1-based unlock attempt this run is trying, carried so a decryption failure can loop
    /// back to a freshly-numbered retry (#72). `1` for a plaintext Backup (no unlock).
    attempt: usize,
}

/// The terminal restore result.
#[derive(Debug)]
enum Report {
    /// Every Entry landed.
    Success { restored: usize },
    /// Best-effort partial: some Entries landed, some failed. `scroll` is the index of the
    /// first failure shown, so a long list scrolls and truncates with `… (+N more)`.
    Partial {
        restored: usize,
        failures: Vec<RestoreFailure>,
        scroll: usize,
    },
    /// The run never produced a per-file report — a wrong-shaped Backup dir, a checksum
    /// mismatch, a decryption failure on a plaintext Backup, or a worker that stopped
    /// unexpectedly. Distinct from [`Report::Partial`]: nothing was restored.
    Fatal { message: String },
}

/// Which screen of the restore flow is live.
#[derive(Debug)]
enum Stage {
    /// The destructive overwrite-confirm; `cancel_focused` starts `true` (Cancel by default).
    Confirm {
        cancel_focused: bool,
    },
    Unlock(Unlock),
    Running(Running),
    Report(Report),
    /// The user closed the flow (Cancel, or dismissing the result). The app reads this and
    /// quits — restore is a standalone route with nothing behind it.
    Exit,
}

/// A message from the restore worker thread to the UI, bridged over an `mpsc` channel.
#[derive(Debug)]
enum Msg {
    Phase(Phase),
    Entry {
        done: usize,
        total: usize,
    },
    /// Boxed to keep the `Msg` enum small — [`RestoreOutcome`] is by far its largest variant.
    Done(Box<RestoreOutcome>),
    Failed(CoreError),
}

/// A [`Progress`] that forwards `core`'s phase/entry callbacks to the UI over the channel.
/// Sends are best-effort: if the UI has quit and dropped the receiver, the worker's
/// notifications simply go nowhere (they are one-way, never consulted — see [`Progress`]).
struct ChannelProgress {
    tx: Sender<Msg>,
}

impl Progress for ChannelProgress {
    fn on_phase(&mut self, phase: Phase) {
        let _ = self.tx.send(Msg::Phase(phase));
    }

    fn on_entry(&mut self, done: usize, total: usize) {
        let _ = self.tx.send(Msg::Entry { done, total });
    }
}

/// The whole restore flow: the immutable run inputs plus the active [`Stage`].
#[derive(Debug)]
pub struct RestoreFlow {
    backup_dir: PathBuf,
    ctx: RunContext,
    encrypted: bool,
    stage: Stage,
}

impl RestoreFlow {
    /// Builds the flow for `backup_dir`, probing (via `core`, ADR-0005 D6) whether the dir is
    /// valid and whether the Backup is encrypted. An invalid dir opens straight on a fatal
    /// report rather than a confirm the user could never satisfy.
    #[must_use]
    pub fn new(backup_dir: PathBuf, ctx: RunContext) -> Self {
        if !fs_probe::is_dir(&backup_dir) {
            let message = format!(
                "Backup dir {} does not exist or is not a directory.",
                backup_dir.display()
            );
            return Self {
                backup_dir,
                ctx,
                encrypted: false,
                stage: Stage::Report(Report::Fatal { message }),
            };
        }
        let encrypted = fs_probe::is_encrypted_backup(&backup_dir);
        Self {
            backup_dir,
            ctx,
            encrypted,
            stage: Stage::Confirm {
                cancel_focused: true,
            },
        }
    }

    /// Whether the user has closed the flow; the app folds this into its quit flag.
    #[must_use]
    pub fn wants_exit(&self) -> bool {
        matches!(self.stage, Stage::Exit)
    }

    /// Whether the worker is running — the one restore state the runtime polls (ADR-0005 D4).
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    /// Folds one key press into the next flow. Pure except for the launch transitions
    /// (Confirm→run and Unlock→run), which spawn the worker thread — the documented RUN-screen
    /// exception to the otherwise side-effect-free reducer.
    #[must_use]
    pub fn handle_key(self, key: KeyEvent) -> Self {
        let Self {
            backup_dir,
            ctx,
            encrypted,
            stage,
        } = self;
        let stage = match stage {
            Stage::Confirm { cancel_focused } => {
                on_confirm_key(cancel_focused, key, &backup_dir, &ctx, encrypted)
            }
            Stage::Unlock(unlock) => on_unlock_key(unlock, key, &backup_dir, &ctx),
            Stage::Report(report) => on_report_key(report, key),
            // Running ignores keys (Ctrl-C is handled by the app as the abort/quit); Exit is
            // terminal.
            other => other,
        };
        Self {
            backup_dir,
            ctx,
            encrypted,
            stage,
        }
    }

    /// Drains any pending worker messages, folding each into the next stage. Called by the
    /// runtime each poll tick while [`is_running`](Self::is_running) holds. The message→screen
    /// logic each drained message runs through ([`fold_msg`]) is pure and unit-tested directly.
    #[must_use]
    pub fn tick(self) -> Self {
        let Self {
            backup_dir,
            ctx,
            encrypted,
            stage,
        } = self;
        let stage = match stage {
            Stage::Running(running) => drain(running, encrypted),
            other => other,
        };
        Self {
            backup_dir,
            ctx,
            encrypted,
            stage,
        }
    }
}

/// Overwrite-confirm key handling. `[ Cancel ]`/`[ Confirm ]` toggle with the arrows or `Tab`;
/// `Enter` activates the focused button. Cancel closes the flow; Confirm advances to unlock
/// (encrypted) or launches the run (plaintext).
fn on_confirm_key(
    cancel_focused: bool,
    key: KeyEvent,
    backup_dir: &Path,
    ctx: &RunContext,
    encrypted: bool,
) -> Stage {
    match key.code {
        KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => Stage::Confirm {
            cancel_focused: !cancel_focused,
        },
        KeyCode::Enter => {
            if cancel_focused {
                Stage::Exit
            } else if encrypted {
                Stage::Unlock(Unlock::new())
            } else {
                Stage::Running(spawn(backup_dir, ctx, None, 1))
            }
        }
        // Esc cancels the dialog — identical to activating Cancel (#61 `Esc cancel`; the keymap's
        // "Esc closes a modal, never quits the app"). The standalone restore route then folds the
        // closed flow into a quit in `App::update`.
        KeyCode::Esc => Stage::Exit,
        _ => Stage::Confirm { cancel_focused },
    }
}

/// Passphrase-unlock key handling. Typing edits the [`SecretInput`] (clearing the stale
/// wrong-passphrase indicator); `F2` toggles reveal; `Enter` on a non-empty field launches the
/// run with the passphrase. An empty `Enter` is a no-op — there is nothing to try yet.
fn on_unlock_key(mut unlock: Unlock, key: KeyEvent, backup_dir: &Path, ctx: &RunContext) -> Stage {
    match key.code {
        KeyCode::F(2) => {
            unlock.reveal = !unlock.reveal;
            Stage::Unlock(unlock)
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            unlock.input.push(ch);
            unlock.wrong = false;
            Stage::Unlock(unlock)
        }
        KeyCode::Backspace => {
            unlock.input.backspace();
            unlock.wrong = false;
            Stage::Unlock(unlock)
        }
        KeyCode::Enter => {
            if unlock.input.is_empty() {
                Stage::Unlock(unlock)
            } else {
                let attempt = unlock.attempt;
                Stage::Running(spawn(
                    backup_dir,
                    ctx,
                    Some(unlock.input.into_secret()),
                    attempt,
                ))
            }
        }
        // Esc cancels the unlock — the #61 `Esc cancel` footer; closes the standalone route.
        KeyCode::Esc => Stage::Exit,
        _ => Stage::Unlock(unlock),
    }
}

/// Result-screen key handling. On a partial report the arrows / page keys scroll the failure
/// list (clamped at both ends); `Enter` closes the flow from any report.
fn on_report_key(report: Report, key: KeyEvent) -> Stage {
    let Report::Partial {
        restored,
        failures,
        scroll,
    } = report
    else {
        return match key.code {
            KeyCode::Enter => Stage::Exit,
            _ => Stage::Report(report),
        };
    };

    let max_scroll = failures.len().saturating_sub(1);
    let scroll = match key.code {
        KeyCode::Enter => return Stage::Exit,
        KeyCode::Up => scroll.saturating_sub(1),
        KeyCode::Down => (scroll + 1).min(max_scroll),
        KeyCode::PageUp | KeyCode::Home => 0,
        KeyCode::PageDown | KeyCode::End => max_scroll,
        _ => scroll,
    };
    Stage::Report(Report::Partial {
        restored,
        failures,
        scroll,
    })
}

/// Spawns the restore worker on a `std::thread`, bridging its [`Progress`] over a fresh channel
/// (ADR-0005 D4: a single background job, no async runtime). The overwrite-confirm the user
/// already cleared *is* the whole-run overwrite consent, so the worker runs with
/// [`CollisionPolicy::Overwrite`]; the UI never re-prompts per collision.
fn spawn(
    backup_dir: &Path,
    ctx: &RunContext,
    passphrase: Option<SecretString>,
    attempt: usize,
) -> Running {
    let (tx, rx) = mpsc::channel();
    let backup_dir = backup_dir.to_path_buf();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let mut progress = ChannelProgress { tx };
        let result = restore::restore(
            &backup_dir,
            &ctx,
            passphrase.as_ref(),
            CollisionPolicy::Overwrite,
            &mut progress,
        );
        let msg = match result {
            Ok(outcome) => Msg::Done(Box::new(outcome)),
            Err(err) => Msg::Failed(err),
        };
        let _ = progress.tx.send(msg);
    });
    Running {
        rx,
        phase: None,
        done: 0,
        total: 0,
        attempt,
    }
}

/// Drains every queued worker message into the running state, transitioning off `Running` on a
/// terminal message. A disconnected channel with no terminal message means the worker thread
/// vanished (a panic) — that reads as a failed run (ADR-0005 D4).
fn drain(mut running: Running, encrypted: bool) -> Stage {
    loop {
        match running.rx.try_recv() {
            Ok(msg) => match fold_msg(running, msg, encrypted) {
                Stage::Running(next) => running = next,
                terminal => return terminal,
            },
            Err(TryRecvError::Empty) => return Stage::Running(running),
            Err(TryRecvError::Disconnected) => {
                return Stage::Report(Report::Fatal {
                    message: "The restore worker stopped unexpectedly.".to_owned(),
                });
            }
        }
    }
}

/// Folds one worker message into the next stage. Progress messages update the running state in
/// place; a terminal message moves to the result screen — except a wrong passphrase on an
/// encrypted Backup, which returns to a fresh unlock screen (unlimited retries, #72).
fn fold_msg(mut running: Running, msg: Msg, encrypted: bool) -> Stage {
    match msg {
        Msg::Phase(phase) => {
            running.phase = Some(phase);
            Stage::Running(running)
        }
        Msg::Entry { done, total } => {
            running.done = done;
            running.total = total;
            Stage::Running(running)
        }
        Msg::Done(outcome) => report_from_outcome(*outcome),
        Msg::Failed(err) => {
            if encrypted && matches!(err, CoreError::DecryptionFailed(_)) {
                Stage::Unlock(Unlock::retry(running.attempt + 1))
            } else {
                Stage::Report(Report::Fatal {
                    message: format!("Restore failed: {err}"),
                })
            }
        }
    }
}

/// Splits a completed [`RestoreOutcome`] into a success or a best-effort partial report
/// (ADR-0002). A declined collision cannot occur here (the worker runs `Overwrite`), so the
/// only non-empty list to surface is `failures`.
fn report_from_outcome(outcome: RestoreOutcome) -> Stage {
    let restored = outcome.restored_paths.len();
    if outcome.failures.is_empty() {
        Stage::Report(Report::Success { restored })
    } else {
        Stage::Report(Report::Partial {
            restored,
            failures: outcome.failures,
            scroll: 0,
        })
    }
}

/// Formats the visible slice of a partial report's failure list for a viewport of `capacity`
/// rows, starting at `scroll`. When more failures remain below than fit, the last visible row
/// becomes the `… (+N more)` truncation line (#72) so an overflowing list stays bounded.
fn partial_lines(failures: &[RestoreFailure], scroll: usize, capacity: usize) -> Vec<String> {
    if capacity == 0 || failures.is_empty() {
        return Vec::new();
    }
    let start = scroll.min(failures.len() - 1);
    let remaining = failures.len() - start;
    if remaining <= capacity {
        return failures[start..].iter().map(format_failure).collect();
    }
    // Reserve the final row for the truncation line rather than dropping a failure silently.
    let shown = capacity - 1;
    let mut lines: Vec<String> = failures[start..start + shown]
        .iter()
        .map(format_failure)
        .collect();
    let more = failures.len() - (start + shown);
    lines.push(format!("… (+{more} more)"));
    lines
}

/// One failed Entry in the partial report: `• <path> — <os error>` verbatim (#72).
fn format_failure(failure: &RestoreFailure) -> String {
    format!("• {} — {}", failure.path, failure.reason)
}

/// The short mode label the restore frame shows in place of app-name chrome (#50).
fn mode_label(flow: &RestoreFlow) -> &'static str {
    match &flow.stage {
        // Exit is transient (the app quits on the same frame), so it shares the confirm label.
        Stage::Confirm { .. } | Stage::Exit => "restore",
        Stage::Unlock(_) => "restore · unlock",
        Stage::Running(_) => "running",
        Stage::Report(Report::Success { .. }) => "done",
        Stage::Report(Report::Partial { .. }) => "done · partial",
        Stage::Report(Report::Fatal { .. }) => "failed",
    }
}

/// Draws the restore flow into `frame`. The sub-`Refuse` too-small notice is handled by the
/// app before dispatch, so this always has a frame to draw into. Header (mode label) and footer
/// (context hints) are fixed; only the body swaps per stage, matching the wizard's stable frame.
pub(crate) fn render(frame: &mut Frame, area: Rect, flow: &RestoreFlow, tier: ReflowTier) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(crate::reflow::MIN_BODY_ROWS),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(mode_label(flow)).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    render_body(frame, chunks[1], flow);
    frame.render_widget(
        Paragraph::new(footer_hints(flow, tier))
            .style(Style::default().add_modifier(Modifier::DIM)),
        chunks[2],
    );
}

fn render_body(frame: &mut Frame, area: Rect, flow: &RestoreFlow) {
    match &flow.stage {
        Stage::Confirm { cancel_focused } => {
            render_confirm(
                frame,
                area,
                &flow.backup_dir,
                &flow.ctx.home,
                flow.encrypted,
                *cancel_focused,
            );
        }
        Stage::Unlock(unlock) => render_unlock(frame, area, unlock),
        Stage::Running(running) => render_running(frame, area, running),
        Stage::Report(report) => render_report(frame, area, report, &flow.ctx.home),
        // Terminal — the app quits on this frame; nothing meaningful to draw.
        Stage::Exit => {}
    }
}

/// The overwrite-confirm body: full-sentence prose (a modal, per the #61 voice rules), the
/// Backup dir, the best-effort caveat, and the two buttons with the focused one marked.
fn render_confirm(
    frame: &mut Frame,
    area: Rect,
    backup_dir: &Path,
    home: &Path,
    encrypted: bool,
    cancel_focused: bool,
) {
    let cancel = focus_button("Cancel", cancel_focused);
    let restore = focus_button("Restore", !cancel_focused);
    let mut lines = vec![
        Line::from("Restore this backup to your system?"),
        Line::from(""),
        Line::from(format!("Backup dir: {}", backup_dir.display())),
        Line::from(format!(
            "This overwrites existing files under {}.",
            home.display()
        )),
        Line::from(
            "Restore is best-effort — any Entry that fails is reported, and the rest continue.",
        ),
    ];
    // Only an encrypted Backup needs the passphrase-required note (#61: shown for encrypted only).
    if encrypted {
        lines.push(Line::from("Encrypted backup — passphrase required."));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![cancel, Span::raw("     "), restore]));
    frame.render_widget(Paragraph::new(lines), area);
}

/// A confirm button, styled and marked when focused. The `> ` marker makes focus assertable in
/// a rendered buffer (styling alone is invisible to a plain cell-symbol check).
fn focus_button(label: &str, focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            format!("> [ {label} ]"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(format!("  [ {label} ]"))
    }
}

fn render_unlock(frame: &mut Frame, area: Rect, unlock: &Unlock) {
    let field = if unlock.reveal {
        unlock.input.exposed().to_owned()
    } else {
        "•".repeat(unlock.input.char_count())
    };
    let mut lines = vec![
        Line::from("Enter passphrase to unlock this backup"),
        Line::from(""),
        Line::from(format!("Passphrase: {field}")),
    ];
    if unlock.wrong {
        lines.push(Line::from(Span::styled(
            format!("✗ wrong passphrase — attempt {}", unlock.attempt),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_running(frame: &mut Frame, area: Rect, running: &Running) {
    let phase = match running.phase {
        Some(Phase::VerifyingArchive) => "Verifying archive",
        Some(Phase::Restoring) => "Restoring",
        // The remaining phases are backup-side; a restore never reports them, but map
        // defensively rather than panic on an unexpected value.
        Some(_) => "Working",
        None => "Starting",
    };
    let mut lines = vec![
        Line::from("Restoring your backup…"),
        Line::from(""),
        Line::from(format!("Phase: {phase}")),
    ];
    if running.total > 0 {
        lines.push(Line::from(format!(
            "Entries: {} / {}",
            running.done, running.total
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_report(frame: &mut Frame, area: Rect, report: &Report, home: &Path) {
    let lines = match report {
        Report::Success { restored } => vec![
            Line::from(Span::styled(
                "✓ Restore complete",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "{restored} Entries restored under {}",
                home.display()
            )),
        ],
        Report::Partial {
            restored,
            failures,
            scroll,
        } => {
            let total = restored + failures.len();
            let mut lines = vec![
                Line::from(Span::styled(
                    format!("⚠ Restore finished with {} failures", failures.len()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{restored} of {total} Entries restored under {}",
                    home.display()
                )),
            ];
            // Reserve the two heading rows above plus a trailing hint row.
            let capacity = usize::from(area.height).saturating_sub(3);
            for text in partial_lines(failures, *scroll, capacity) {
                lines.push(Line::from(text));
            }
            lines
        }
        Report::Fatal { message } => vec![
            Line::from(Span::styled(
                "✗ Restore failed",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(message.clone()),
        ],
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), area);
}

/// The footer hint fragments per stage (lowercase, terse — #61), always keeping `? keys`.
fn footer_hints(flow: &RestoreFlow, _tier: ReflowTier) -> String {
    match &flow.stage {
        Stage::Confirm { .. } => "←→ choose · Enter confirm · Esc cancel · ? keys".to_owned(),
        Stage::Unlock(unlock) => {
            let reveal = if unlock.reveal {
                "F2 hide"
            } else {
                "F2 reveal"
            };
            format!("Enter unlock · {reveal} · Esc cancel · ? keys")
        }
        Stage::Running(_) => "Ctrl-C cancel · ? keys".to_owned(),
        Stage::Report(Report::Partial { .. }) => "↑/↓ scroll · Enter close · ? keys".to_owned(),
        Stage::Report(_) => "Enter close · ? keys".to_owned(),
        Stage::Exit => "? keys".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use linux_backup_core::Privilege;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::SystemTime;

    fn test_context() -> RunContext {
        RunContext {
            home: PathBuf::from("/home/tester"),
            output_location: PathBuf::from("/home/tester"),
            privilege: Privilege::NonRoot,
            host: "test-host".to_owned(),
            user: "tester".to_owned(),
            uid: 1000,
            os: "test-os".to_owned(),
            now: SystemTime::UNIX_EPOCH,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    /// Builds a flow directly in a chosen stage, bypassing the FS-probing constructor so
    /// reducer/render tests are deterministic and TTY-free.
    fn flow_in(stage: Stage, encrypted: bool) -> RestoreFlow {
        RestoreFlow {
            backup_dir: PathBuf::from("/backups/test"),
            ctx: test_context(),
            encrypted,
            stage,
        }
    }

    fn failures(count: usize) -> Vec<RestoreFailure> {
        (0..count)
            .map(|i| RestoreFailure {
                path: format!("/home/tester/file-{i}"),
                reason: format!("os error {i}"),
            })
            .collect()
    }

    fn rendered_lines(flow: &RestoreFlow, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), flow, ReflowTier::for_height(height)))
            .expect("draw into test backend");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn confirm_focuses_cancel_by_default() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: true,
            },
            false,
        );
        assert!(matches!(
            flow.stage,
            Stage::Confirm {
                cancel_focused: true
            }
        ));
    }

    #[test]
    fn confirm_cancel_enter_closes_the_flow() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: true,
            },
            false,
        );
        let flow = flow.handle_key(key(KeyCode::Enter));
        assert!(flow.wants_exit());
    }

    #[test]
    fn confirm_esc_cancels_the_flow() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: false,
            },
            false,
        );
        // Esc cancels regardless of which button is focused (#61 `Esc cancel`).
        assert!(flow.handle_key(key(KeyCode::Esc)).wants_exit());
    }

    #[test]
    fn unlock_esc_cancels_the_flow() {
        let flow = flow_in(Stage::Unlock(Unlock::new()), true);
        assert!(flow.handle_key(key(KeyCode::Esc)).wants_exit());
    }

    #[test]
    fn confirm_confirm_on_encrypted_backup_opens_unlock() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: true,
            },
            true,
        );
        // Move focus off Cancel onto Confirm, then activate it.
        let flow = flow
            .handle_key(key(KeyCode::Right))
            .handle_key(key(KeyCode::Enter));
        assert!(matches!(flow.stage, Stage::Unlock(_)));
    }

    #[test]
    fn confirm_confirm_on_plaintext_backup_launches_the_run() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: false,
            },
            false,
        );
        let flow = flow.handle_key(key(KeyCode::Enter));
        assert!(flow.is_running());
    }

    #[test]
    fn unlock_typing_masks_and_clears_the_wrong_indicator() {
        let flow = flow_in(Stage::Unlock(Unlock::retry(2)), true);
        let flow = flow
            .handle_key(key(KeyCode::Char('p')))
            .handle_key(key(KeyCode::Char('w')));
        let Stage::Unlock(unlock) = &flow.stage else {
            panic!("expected unlock stage");
        };
        assert_eq!(unlock.input.char_count(), 2);
        assert!(!unlock.wrong, "typing should clear the stale indicator");
    }

    #[test]
    fn unlock_f2_toggles_reveal_only() {
        let flow = flow_in(Stage::Unlock(Unlock::new()), true).handle_key(key(KeyCode::F(2)));
        let Stage::Unlock(unlock) = &flow.stage else {
            panic!("expected unlock stage");
        };
        assert!(unlock.reveal);
    }

    #[test]
    fn unlock_empty_enter_is_a_no_op() {
        let flow = flow_in(Stage::Unlock(Unlock::new()), true).handle_key(key(KeyCode::Enter));
        assert!(matches!(flow.stage, Stage::Unlock(_)));
    }

    #[test]
    fn unlock_enter_with_a_passphrase_launches_the_run() {
        let flow = flow_in(Stage::Unlock(Unlock::new()), true)
            .handle_key(key(KeyCode::Char('s')))
            .handle_key(key(KeyCode::Enter));
        assert!(flow.is_running());
    }

    #[test]
    fn wrong_passphrase_returns_to_unlock_with_the_indicator_lit() {
        // A running encrypted restore whose worker reports a decryption failure loops back to a
        // fresh unlock screen — unlimited retries (#72), never a lock-out.
        let running = spawn_stub();
        let stage = fold_msg(
            running,
            Msg::Failed(CoreError::DecryptionFailed("bad passphrase".to_owned())),
            true,
        );
        let Stage::Unlock(unlock) = stage else {
            panic!("expected a loop back to unlock");
        };
        assert!(unlock.wrong);
        assert_eq!(unlock.attempt, 2, "the failed attempt bumps the counter");
        assert!(
            unlock.input.is_empty(),
            "the retry starts from a blank field"
        );
    }

    #[test]
    fn retry_after_wrong_passphrase_can_launch_again() {
        // Proves the retry loop has no cap: a second attempt launches exactly like the first.
        let flow = flow_in(Stage::Unlock(Unlock::retry(2)), true)
            .handle_key(key(KeyCode::Char('x')))
            .handle_key(key(KeyCode::Enter));
        assert!(flow.is_running());
    }

    #[test]
    fn decryption_failure_on_a_plaintext_backup_is_fatal_not_a_retry() {
        let running = spawn_stub();
        let stage = fold_msg(
            running,
            Msg::Failed(CoreError::DecryptionFailed("x".to_owned())),
            false,
        );
        assert!(matches!(stage, Stage::Report(Report::Fatal { .. })));
    }

    #[test]
    fn done_with_no_failures_is_a_success_report() {
        let stage = report_from_outcome(outcome_with(vec![PathBuf::from("/a")], Vec::new()));
        assert!(matches!(
            stage,
            Stage::Report(Report::Success { restored: 1 })
        ));
    }

    #[test]
    fn done_with_failures_is_a_partial_report() {
        let stage = report_from_outcome(outcome_with(vec![PathBuf::from("/a")], failures(2)));
        assert!(matches!(
            stage,
            Stage::Report(Report::Partial { restored: 1, .. })
        ));
    }

    #[test]
    fn partial_report_scroll_clamps_at_both_ends() {
        let report = Report::Partial {
            restored: 0,
            failures: failures(3),
            scroll: 0,
        };
        // Up at the top stays at the top.
        let stage = on_report_key(report, key(KeyCode::Up));
        let scroll = partial_scroll(&stage);
        assert_eq!(scroll, 0);

        // Down past the end clamps to len-1.
        let mut stage = Stage::Report(Report::Partial {
            restored: 0,
            failures: failures(3),
            scroll: 0,
        });
        for _ in 0..10 {
            let Stage::Report(report) = stage else {
                panic!("expected partial report");
            };
            stage = on_report_key(report, key(KeyCode::Down));
        }
        assert_eq!(partial_scroll(&stage), 2);
    }

    #[test]
    fn partial_lines_truncates_overflow_with_a_more_line() {
        let items = failures(10);
        let lines = partial_lines(&items, 0, 4);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("• /home/tester/file-0 — os error 0"));
        assert_eq!(lines[3], "… (+7 more)");
    }

    #[test]
    fn partial_lines_shows_all_when_they_fit() {
        let items = failures(3);
        let lines = partial_lines(&items, 0, 8);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| !l.contains("more")));
    }

    #[test]
    fn report_enter_closes_the_flow() {
        let flow = flow_in(Stage::Report(Report::Success { restored: 3 }), false);
        assert!(flow.handle_key(key(KeyCode::Enter)).wants_exit());
    }

    // --- render (TestBackend) --------------------------------------------------------------

    #[test]
    fn renders_overwrite_confirm_with_cancel_focused() {
        let flow = flow_in(
            Stage::Confirm {
                cancel_focused: true,
            },
            false,
        );
        let lines = rendered_lines(&flow, 70, 16);
        assert!(
            lines.iter().any(|l| l.contains("> [ Cancel ]")),
            "Cancel must be focused by default, got: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("existing files") || l.contains("Existing files")),
            "the one screen allowed the 'existing files' wording, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("best-effort")),
            "must state restore is best-effort, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("/backups/test")),
            "must state what will be written, got: {lines:?}"
        );
    }

    #[test]
    fn renders_unlock_with_the_wrong_passphrase_indicator() {
        let flow = flow_in(Stage::Unlock(Unlock::retry(2)), true);
        let lines = rendered_lines(&flow, 70, 12);
        assert!(
            lines.iter().any(|l| l.contains("Passphrase:")),
            "expected the passphrase field, got: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("wrong passphrase — attempt 2")),
            "expected the numbered wrong-passphrase indicator, got: {lines:?}"
        );
    }

    #[test]
    fn renders_success_report() {
        let flow = flow_in(Stage::Report(Report::Success { restored: 42 }), false);
        let lines = rendered_lines(&flow, 60, 12);
        assert!(lines.iter().any(|l| l.contains("Restore complete")));
        assert!(lines
            .iter()
            .any(|l| l.contains("42 Entries restored under")));
    }

    #[test]
    fn renders_partial_report_with_the_more_truncation() {
        let flow = flow_in(
            Stage::Report(Report::Partial {
                restored: 1,
                failures: failures(20),
                scroll: 0,
            }),
            false,
        );
        let lines = rendered_lines(&flow, 70, 12);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("• /home/tester/file-0 — os error 0")),
            "expected the `• <path> — <error>` failure format, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("more)")),
            "expected the `… (+N more)` truncation, got: {lines:?}"
        );
    }

    // --- test helpers ----------------------------------------------------------------------

    /// A `Running` state whose worker never sends anything — enough to exercise `fold_msg`
    /// without a real restore. The dropped sender is fine: `fold_msg` reads only the message
    /// passed to it, not the channel.
    fn spawn_stub() -> Running {
        let (_tx, rx) = mpsc::channel();
        Running {
            rx,
            phase: None,
            done: 0,
            total: 0,
            attempt: 1,
        }
    }

    fn outcome_with(restored: Vec<PathBuf>, failures: Vec<RestoreFailure>) -> RestoreOutcome {
        use linux_backup_core::manifest::Manifest;
        use linux_backup_core::restore::OwnershipOutcome;
        use std::collections::BTreeMap;
        RestoreOutcome {
            manifest: Manifest {
                schema_version: linux_backup_core::manifest::SCHEMA_VERSION,
                tool_version: "test".to_owned(),
                created_at: "1970-01-01T00:00:00Z".to_owned(),
                host: "test-host".to_owned(),
                user: "tester".to_owned(),
                uid: 1000,
                home: PathBuf::from("/home/tester"),
                os: "test-os".to_owned(),
                entries: Vec::new(),
                checksums: BTreeMap::new(),
                archive_sha256: "deadbeef".to_owned(),
                vanished: Vec::new(),
                packages: BTreeMap::new(),
            },
            restored_paths: restored,
            failures,
            skipped_collisions: Vec::new(),
            ownership: OwnershipOutcome::SkippedNonRoot,
            reinstall_script_path: PathBuf::from("/home/tester/reinstall.sh"),
            package_counts: BTreeMap::new(),
        }
    }

    fn partial_scroll(stage: &Stage) -> usize {
        match stage {
            Stage::Report(Report::Partial { scroll, .. }) => *scroll,
            _ => panic!("expected a partial report"),
        }
    }
}
