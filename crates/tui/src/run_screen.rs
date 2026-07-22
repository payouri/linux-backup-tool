//! The RUN screen: one of the two polling screen (the other being the running restore screen),
//! its worker-thread bridge, and its widget state.
//!
//! Per ADR-0005 D4 / spec #62, `core::backup` runs unchanged on a `std::thread` worker; a
//! tui-side [`ChannelProgress`] forwards its `on_phase`/`on_entry` callbacks over a
//! `std::sync::mpsc` channel as [`RunMessage`]s. The runtime ([`crate::runtime`]) polls that
//! channel and crossterm together each tick during a running backup — the only screen that
//! polls — and folds each message into this module's [`RunScreen`] widget state, which the
//! pure render then draws into the app frame. A terminal [`RunMessage::Done`]/[`RunMessage::Failed`]
//! ends the run; a worker panic disconnects the channel and reads as a failed run
//! ([`RunScreen::on_worker_disconnected`]). No `tokio`, no async `EventStream` — a single
//! background job does not warrant an async runtime.
//!
//! The thread spawn and channel are the system-boundary edge (kept thin, off the automated
//! seam); the phase folding ([`RunScreen::apply`]) and the render are the pure, unit-tested
//! parts.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use linux_backup_core::archive::Encryption;
use linux_backup_core::model::SelectionModel;
use linux_backup_core::packages::SystemCommandRunner;
use linux_backup_core::progress::{Phase, Progress};
use linux_backup_core::{backup, crypto, BackupOutcome, CoreError, RunContext};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::glyphs::Glyphs;
use crate::reflow::{ReflowTier, MIN_BODY_ROWS};

/// The self-contained backup inputs the runtime hands to the worker thread. Everything is
/// owned (cloned out of the wizard at launch) so the worker outlives the wizard view it came
/// from and needs no borrow back into `App`.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    pub selection: SelectionModel,
    pub ctx: RunContext,
    pub encryption: Encryption,
}

/// One message from the worker: a progress notification, or the terminal outcome. Folded into
/// [`RunScreen`] by [`RunScreen::apply`]. `Done` boxes its [`BackupOutcome`] to keep the enum
/// small (the outcome carries a whole Manifest).
#[derive(Debug)]
pub enum RunMessage {
    Phase(Phase),
    Entry {
        done: usize,
        total: usize,
    },
    Done(Box<BackupOutcome>),
    /// A `CoreError`'s rendered message plus whether the run's fail-fast cleanup ran — the
    /// worker maps the typed error to its `Display` string at the thread boundary so the
    /// channel carries a plain, `Send` payload. `cleaned_up` is decided from the error variant
    /// there too, since most `CoreError`s run ADR-0002 cleanup but a collision refusal
    /// (`BackupDirExists`) touched nothing to clean.
    Failed {
        message: String,
        cleaned_up: bool,
    },
}

/// The worker handle the runtime holds while a backup is in flight: the receiving end of the
/// progress channel plus the join handle (kept alive so the thread isn't detached mid-run).
pub struct BackupWorker {
    rx: Receiver<RunMessage>,
    _handle: JoinHandle<()>,
}

impl BackupWorker {
    /// Non-blocking poll of the progress channel — the runtime calls this each tick.
    pub fn try_recv(&self) -> Result<RunMessage, TryRecvError> {
        self.rx.try_recv()
    }
}

/// Spawns the backup worker. `core::backup` runs unchanged on the thread with a
/// [`ChannelProgress`] adapter; whatever it returns is sent as the terminal [`RunMessage`],
/// then the sender drops and the channel disconnects (a normal completion the runtime sees
/// after the terminal message; a panic before it, the runtime reads as a failed run).
pub fn spawn(req: BackupRequest) -> BackupWorker {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut progress = ChannelProgress { tx: tx.clone() };
        let runner = SystemCommandRunner;
        let message = match backup(
            &req.selection,
            &req.ctx,
            &req.encryption,
            &runner,
            &mut progress,
        ) {
            Ok(outcome) => RunMessage::Done(Box::new(outcome)),
            Err(error) => RunMessage::Failed {
                // A collision refusal never created a Backup dir, so no partial backup was
                // removed; every other `CoreError` fires after the fresh dir exists and runs
                // ADR-0002 fail-fast cleanup.
                cleaned_up: !matches!(error, CoreError::BackupDirExists(_)),
                message: error.to_string(),
            },
        };
        // A send failure means the UI already tore down (quit); nothing to do but exit.
        let _ = tx.send(message);
    });
    BackupWorker {
        rx,
        _handle: handle,
    }
}

/// The [`Progress`] implementation the worker drives: every `core` callback becomes a
/// [`RunMessage`] on the channel. Sends are best-effort — if the receiver is gone (the UI
/// quit), `core` neither blocks nor sees an error, matching the trait's one-way contract.
struct ChannelProgress {
    tx: mpsc::Sender<RunMessage>,
}

impl Progress for ChannelProgress {
    fn on_phase(&mut self, phase: Phase) {
        let _ = self.tx.send(RunMessage::Phase(phase));
    }

    fn on_entry(&mut self, done: usize, total: usize) {
        let _ = self.tx.send(RunMessage::Entry { done, total });
    }
}

/// The static run facts captured at launch — the pieces the RUN and success screens show that
/// aren't in the worker's messages: where the Backup dir lands, whether it's encrypted, the
/// Sensitive Categories shipping in plaintext (for the persistent warning), the total raw
/// Selection size, and the Entry count (the Reading bar's total until the worker reports one).
#[derive(Debug, Clone)]
pub struct RunPlan {
    backup_dir: PathBuf,
    encrypted: bool,
    plaintext_sensitive_categories: Vec<String>,
    selection_size: u64,
    entry_count: usize,
}

impl RunPlan {
    /// Derives the plan from the launched Selection, encryption choice, and the context whose
    /// `output_location` was already pointed at the chosen Backup location.
    #[must_use]
    pub fn new(selection: &SelectionModel, encryption: &Encryption, ctx: &RunContext) -> Self {
        let encrypted = encryption.is_encrypted();
        // Only a plaintext run can expose Sensitive content; an encrypted run never does, so
        // the warning list is empty there.
        let plaintext_sensitive_categories = if encrypted {
            Vec::new()
        } else {
            crypto::evaluate(selection)
                .sensitive_categories
                .into_iter()
                .map(str::to_string)
                .collect()
        };
        Self {
            backup_dir: ctx
                .output_location
                .join(linux_backup_core::backup_dir_name(ctx)),
            encrypted,
            plaintext_sensitive_categories,
            selection_size: selection.total_size(),
            entry_count: selection.entries().len(),
        }
    }
}

/// The live per-phase progress while the worker runs: the current [`Phase`] (`None` until the
/// first one arrives), the per-Entry counts (shown as the Reading bar), and the spinner tick.
#[derive(Debug, Clone, Default)]
struct RunProgress {
    phase: Option<Phase>,
    entry_done: usize,
    entry_total: usize,
    spinner_tick: usize,
}

/// Where the run is: still working (with live progress), or terminal (a completed
/// [`BackupOutcome`] or a failure).
#[derive(Debug)]
enum RunActivity {
    Running(RunProgress),
    Done(Box<BackupOutcome>),
    Failed(RunFailure),
}

/// A terminal failure: the primary line (a `CoreError`'s message, or the worker-stopped
/// notice) and whether the fail-fast cleanup left nothing behind. Most `CoreError`s run
/// ADR-0002 cleanup, but a collision refusal created nothing to clean and an abandoned worker
/// (panic/disconnect) can't promise cleanup ran at all.
#[derive(Debug)]
struct RunFailure {
    message: String,
    cleaned_up: bool,
}

/// The RUN screen's whole state: the static plan plus where the run is.
#[derive(Debug)]
pub struct RunScreen {
    plan: RunPlan,
    activity: RunActivity,
}

impl RunScreen {
    /// Opens the screen the instant a launch is requested — before the worker's first message,
    /// so the frame paints "Backing up…" immediately rather than a blank tick.
    #[must_use]
    pub fn launching(plan: RunPlan) -> Self {
        Self {
            plan,
            activity: RunActivity::Running(RunProgress::default()),
        }
    }

    /// Whether the backup is still in flight — the runtime polls while this holds and blocks on
    /// input once it's terminal (the success/failure screen waits for `Enter` to close).
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.activity, RunActivity::Running(_))
    }

    /// Whether the run has ended (success or failure) — the point `Enter` closes the screen.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !self.is_running()
    }

    /// Folds one worker message into the state. Progress messages update the live phase/counts;
    /// a terminal message flips the activity. Messages after a terminal one are ignored (the
    /// worker sends exactly one terminal message, but a late progress send mustn't resurrect a
    /// finished run).
    pub fn apply(&mut self, message: RunMessage) {
        let RunActivity::Running(progress) = &mut self.activity else {
            return;
        };
        match message {
            RunMessage::Phase(phase) => progress.phase = Some(phase),
            RunMessage::Entry { done, total } => {
                progress.entry_done = done;
                progress.entry_total = total;
            }
            RunMessage::Done(outcome) => self.activity = RunActivity::Done(outcome),
            RunMessage::Failed {
                message,
                cleaned_up,
            } => {
                self.activity = RunActivity::Failed(RunFailure {
                    message,
                    cleaned_up,
                });
            }
        }
    }

    /// The channel disconnected without a terminal message — the worker panicked. Reads as a
    /// failed run; unlike a `CoreError` return, cleanup can't be promised here.
    pub fn on_worker_disconnected(&mut self) {
        if self.is_running() {
            self.activity = RunActivity::Failed(RunFailure {
                message: "The backup stopped unexpectedly.".to_string(),
                cleaned_up: false,
            });
        }
    }

    /// Advances the spinner one frame — the runtime calls this each poll tick so an
    /// indeterminate phase animates even when no message arrived this tick.
    pub fn tick(&mut self) {
        if let RunActivity::Running(progress) = &mut self.activity {
            progress.spinner_tick = progress.spinner_tick.wrapping_add(1);
        }
    }
}

/// The RUN mode label (#50: a short mode label in place of the wizard stepper).
fn mode_label(activity: &RunActivity) -> &'static str {
    match activity {
        RunActivity::Running(_) => "running",
        RunActivity::Done(_) => "done",
        RunActivity::Failed(_) => "failed",
    }
}

/// Draws the RUN screen into the app frame: the mode label, the body (progress, or the
/// success/failure summary), and the one-line footer. The `Refuse` cutoff is handled upstream
/// in [`crate::app::App::render`], so this only ever draws a laid-out frame.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &RunScreen,
    _tier: ReflowTier,
    glyphs: Glyphs,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(MIN_BODY_ROWS),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(mode_label(&screen.activity))
            .style(Style::default().add_modifier(Modifier::DIM)),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(body_lines(screen, glyphs)), chunks[1]);
    frame.render_widget(
        Paragraph::new(footer_hint(&screen.activity))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        chunks[2],
    );
}

/// The footer hint: `Ctrl-C cancel` while running (the user needs only that Ctrl-C cancels),
/// `Enter close` once terminal (#61 copy). Both keep the `F1 keys` pointer to the exhaustive
/// overlay that the footer always carries on every screen (#51).
fn footer_hint(activity: &RunActivity) -> &'static str {
    match activity {
        RunActivity::Running(_) => "Ctrl-C cancel · F1 keys",
        _ => "Enter close · F1 keys",
    }
}

/// The body content for the current activity.
fn body_lines(screen: &RunScreen, glyphs: Glyphs) -> Vec<Line<'static>> {
    match &screen.activity {
        RunActivity::Running(progress) => running_lines(&screen.plan, progress, glyphs),
        RunActivity::Done(outcome) => success_lines(&screen.plan, outcome, glyphs),
        RunActivity::Failed(failure) => failure_lines(failure, glyphs),
    }
}

/// The in-progress body: heading, the phase row, and either the per-Entry Reading bar or the
/// indeterminate-phase spinner (#61 copy).
fn running_lines(plan: &RunPlan, progress: &RunProgress, glyphs: Glyphs) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Backing up…"),
        Line::default(),
        phase_row(plan.encrypted, progress.phase, glyphs),
        Line::default(),
    ];
    // The per-Entry bar belongs to Reading (core's `Scanning`); every other phase is
    // indeterminate and shows the spinner (#61).
    if progress.phase == Some(Phase::Scanning) {
        // Until the worker reports a total, fall back to the Entry count known at launch so
        // the bar reads `0 / 128` rather than a bare `0 / 0`.
        let total = if progress.entry_total > 0 {
            progress.entry_total
        } else {
            plan.entry_count
        };
        lines.push(Line::from(reading_bar(progress.entry_done, total)));
    } else if let Some(phase) = progress.phase {
        let spin = glyphs.spinner[progress.spinner_tick % glyphs.spinner.len()];
        lines.push(Line::from(format!("{spin} {}…", phase_label(phase))));
    }
    lines
}

/// The phase row `✓ Reading  ● Compressing  · Finalizing` — the display phases in order, each
/// glyphed by position relative to the current phase (done / current / pending), through the
/// active glyph set (#73). Encrypting is omitted entirely for a plaintext run (#65/#61), so it
/// never appears when `!encrypted`.
fn phase_row(encrypted: bool, current: Option<Phase>, glyphs: Glyphs) -> Line<'static> {
    let current_rank = current.and_then(phase_rank);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for phase in display_phases(encrypted) {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let rank = phase_rank(phase);
        let (glyph, style) = match (current_rank, rank) {
            (Some(cur), Some(this)) if this < cur => {
                (glyphs.check, Style::default().fg(Color::Green))
            }
            (Some(cur), Some(this)) if this == cur => (
                glyphs.current,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            _ => (glyphs.locked, Style::default().add_modifier(Modifier::DIM)),
        };
        spans.push(Span::styled(
            format!("{glyph} {}", phase_label(phase)),
            style,
        ));
    }
    Line::from(spans)
}

/// The display phase sequence: Reading → Compressing → (Encrypting) → Finalizing, with
/// Encrypting present only when the run encrypts (#61).
fn display_phases(encrypted: bool) -> Vec<Phase> {
    if encrypted {
        vec![
            Phase::Scanning,
            Phase::Compressing,
            Phase::Encrypting,
            Phase::Finalizing,
        ]
    } else {
        vec![Phase::Scanning, Phase::Compressing, Phase::Finalizing]
    }
}

/// Ordinal of a phase within the backup display sequence, for the done/current/pending
/// comparison. Restore-only phases have no place in the backup row and rank as `None`.
fn phase_rank(phase: Phase) -> Option<usize> {
    match phase {
        Phase::Scanning => Some(0),
        Phase::Compressing => Some(1),
        Phase::Encrypting => Some(2),
        Phase::Finalizing => Some(3),
        Phase::VerifyingArchive | Phase::Restoring => None,
    }
}

/// The RUN display label for each backup phase (`Scanning` shows as Reading, #61).
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Scanning => "Reading",
        Phase::Compressing => "Compressing",
        Phase::Encrypting => "Encrypting",
        Phase::Finalizing => "Finalizing",
        Phase::VerifyingArchive | Phase::Restoring => "Working",
    }
}

/// The per-Entry Reading bar: `[====>     ] 216 / 412 Entries`. A zero total (before the first
/// count) renders an empty bar rather than dividing by zero.
fn reading_bar(done: usize, total: usize) -> String {
    const WIDTH: usize = 24;
    // A zero total (before the first count) yields no fill rather than dividing by zero.
    let filled = done.saturating_mul(WIDTH).checked_div(total).unwrap_or(0);
    let bar: String = std::iter::repeat_n('=', filled)
        .chain(std::iter::repeat_n(' ', WIDTH - filled))
        .collect();
    format!("[{bar}] {done} / {total} Entries")
}

/// The success summary (#61 copy): Backup dir, the Entry-count · size · encryption-tag line,
/// per-manager Package counts, the vanished callout only when count > 0, and the persistent
/// plaintext-Sensitive warning when Sensitive content shipped unencrypted.
fn success_lines(plan: &RunPlan, outcome: &BackupOutcome, glyphs: Glyphs) -> Vec<Line<'static>> {
    let manifest = &outcome.manifest;
    let tag = if plan.encrypted {
        "encrypted"
    } else {
        "not encrypted"
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} Backup complete", glyphs.check),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!("Backup dir   {}", plan.backup_dir.display())),
        Line::from(format!(
            "{} Entries · {} · {tag}",
            manifest.entries.len(),
            format_size(plan.selection_size),
        )),
    ];

    if !manifest.packages.is_empty() {
        let counts = manifest
            .packages
            .iter()
            .map(|(manager, packages)| format!("{manager} {}", packages.len()))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(format!("Packages     {counts}")));
    }

    if !manifest.vanished.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "{} {} Entries vanished, skipped — recorded in the Manifest",
                glyphs.warn,
                manifest.vanished.len()
            ),
            Style::default().fg(Color::Yellow),
        )));
    }

    if !plan.plaintext_sensitive_categories.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "{} not encrypted — {} in plaintext",
                glyphs.warn,
                plan.plaintext_sensitive_categories.join(", ")
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }

    lines
}

/// The failure screen (#61 copy): the error line, then the ADR-0002 reassurance that a
/// fail-fast run left nothing behind (or, on an abandoned worker, that completeness can't be
/// promised).
fn failure_lines(failure: &RunFailure, glyphs: Glyphs) -> Vec<Line<'static>> {
    let reassurance = if failure.cleaned_up {
        "Nothing was written — the partial backup was removed."
    } else {
        "The backup may be incomplete — check the Output location."
    };
    vec![
        Line::from(Span::styled(
            format!("{} Backup failed", glyphs.cross),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(failure.message.clone()),
        Line::from(reassurance),
    ]
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based). Mirrors the other screens' size
/// rendering so every screen reads sizes the same way; kept local rather than shared to avoid
/// coupling the screen modules.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    #[allow(clippy::cast_precision_loss)]
    let bytes_f = bytes as f64;
    if bytes_f >= GB {
        format!("{:.2} GB", bytes_f / GB)
    } else if bytes_f >= MB {
        format!("{:.2} MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.2} KB", bytes_f / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linux_backup_core::manifest::{Manifest, PackageInfo, VanishedEntry, SCHEMA_VERSION};
    use linux_backup_core::model::{Entry, EntryRoot, EntryType};
    use linux_backup_core::Privilege;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    fn ctx() -> RunContext {
        RunContext {
            home: PathBuf::from("/home/tester"),
            output_location: PathBuf::from("/run/media/tester/USB"),
            privilege: Privilege::NonRoot,
            host: "thinkpad".to_string(),
            user: "tester".to_string(),
            uid: 1000,
            os: "test-os".to_string(),
            now: SystemTime::UNIX_EPOCH,
        }
    }

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/home/tester/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive,
        }
    }

    fn plaintext_plan() -> RunPlan {
        let selection = SelectionModel::new(vec![entry("Shell", false, ".zshrc")]);
        RunPlan::new(&selection, &Encryption::None, &ctx())
    }

    fn encrypted_plan() -> RunPlan {
        use secrecy::SecretString;
        let selection = SelectionModel::new(vec![entry("SSH", true, ".ssh/id_ed25519")]);
        RunPlan::new(
            &selection,
            &Encryption::Passphrase(SecretString::from("hunter2hunter2!")),
            &ctx(),
        )
    }

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
            entries: (0..entries)
                .map(|i| {
                    linux_backup_core::manifest::ManifestEntry::from(&entry(
                        "Shell",
                        false,
                        &format!("f{i}"),
                    ))
                })
                .collect(),
            checksums: BTreeMap::new(),
            archive_sha256: "deadbeef".to_string(),
            vanished,
            packages,
        }
    }

    fn rendered(screen: &RunScreen, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    screen,
                    ReflowTier::Default,
                    Glyphs::UNICODE,
                );
            })
            .expect("draw");
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
    fn phase_folds_into_the_current_phase() {
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.apply(RunMessage::Phase(Phase::Scanning));
        screen.apply(RunMessage::Entry { done: 3, total: 10 });
        // Still running, on Reading, with the counts folded in.
        assert!(screen.is_running());
        let RunActivity::Running(progress) = &screen.activity else {
            panic!("expected a running activity");
        };
        assert_eq!(progress.phase, Some(Phase::Scanning));
        assert_eq!(progress.entry_done, 3);
        assert_eq!(progress.entry_total, 10);
    }

    #[test]
    fn plaintext_run_omits_the_encrypting_phase() {
        let row = phase_row(false, Some(Phase::Compressing), Glyphs::UNICODE);
        let text: String = row
            .spans
            .iter()
            .map(ratatui::text::Span::to_string)
            .collect();
        assert!(text.contains("Reading"));
        assert!(text.contains("Compressing"));
        assert!(text.contains("Finalizing"));
        assert!(
            !text.contains("Encrypting"),
            "plaintext run must not show Encrypting, got: {text:?}"
        );
    }

    #[test]
    fn encrypted_run_shows_the_encrypting_phase() {
        let row = phase_row(true, Some(Phase::Encrypting), Glyphs::UNICODE);
        let text: String = row
            .spans
            .iter()
            .map(ratatui::text::Span::to_string)
            .collect();
        assert!(text.contains("Encrypting"));
        // Encrypting is current (●); Reading/Compressing before it are done (✓).
        assert!(text.contains('●'));
        assert!(text.contains('✓'));
    }

    #[test]
    fn done_message_transitions_to_the_success_screen() {
        let mut screen = RunScreen::launching(plaintext_plan());
        let outcome = BackupOutcome {
            backup_dir: PathBuf::from("/run/media/tester/USB/linux-backup-thinkpad-0"),
            manifest: manifest_with(5, Vec::new(), BTreeMap::new()),
        };
        screen.apply(RunMessage::Done(Box::new(outcome)));
        assert!(screen.is_terminal());
        let lines = rendered(&screen, 70, 14);
        assert!(lines.iter().any(|l| l.contains("Backup complete")));
        assert!(lines.iter().any(|l| l.contains("5 Entries")));
        assert!(lines.iter().any(|l| l.contains("not encrypted")));
        assert!(lines.iter().any(|l| l.contains("done")));
        assert!(lines.iter().any(|l| l.contains("Enter close")));
        // The footer always keeps the `F1 keys` pointer, on the RUN screen like every other (#51).
        assert!(lines.iter().any(|l| l.contains("F1 keys")));
    }

    #[test]
    fn failed_message_transitions_to_the_failure_screen() {
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.apply(RunMessage::Failed {
            message: "failed to read /home/tester/.ssh/id_ed25519: permission denied".to_string(),
            cleaned_up: true,
        });
        assert!(screen.is_terminal());
        let lines = rendered(&screen, 70, 14);
        assert!(lines.iter().any(|l| l.contains("Backup failed")));
        assert!(lines.iter().any(|l| l.contains("permission denied")));
        assert!(lines
            .iter()
            .any(|l| l.contains("the partial backup was removed")));
        assert!(lines.iter().any(|l| l.contains("failed")));
    }

    #[test]
    fn a_collision_failure_does_not_claim_the_partial_backup_was_removed() {
        // A `BackupDirExists` refusal created nothing, so the reassurance must not claim a
        // partial backup was removed (which would read as "your existing Backup dir was
        // deleted").
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.apply(RunMessage::Failed {
            message: "backup dir already exists: /run/media/tester/USB/linux-backup-thinkpad-0"
                .to_string(),
            cleaned_up: false,
        });
        assert!(screen.is_terminal());
        let lines = rendered(&screen, 70, 14);
        assert!(lines.iter().any(|l| l.contains("Backup failed")));
        assert!(lines.iter().any(|l| l.contains("already exists")));
        assert!(
            !lines.iter().any(|l| l.contains("was removed")),
            "a collision refusal must not claim the partial backup was removed"
        );
    }

    #[test]
    fn a_worker_disconnect_reads_as_a_failed_run_without_promising_cleanup() {
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.on_worker_disconnected();
        assert!(screen.is_terminal());
        let lines = rendered(&screen, 70, 14);
        assert!(lines.iter().any(|l| l.contains("Backup failed")));
        assert!(lines.iter().any(|l| l.contains("stopped unexpectedly")));
        assert!(
            !lines.iter().any(|l| l.contains("was removed")),
            "an abandoned worker must not claim the partial backup was removed"
        );
    }

    #[test]
    fn a_terminal_run_ignores_late_progress_messages() {
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.apply(RunMessage::Failed {
            message: "boom".to_string(),
            cleaned_up: true,
        });
        // A stray late progress send must not resurrect the finished run.
        screen.apply(RunMessage::Phase(Phase::Finalizing));
        assert!(matches!(screen.activity, RunActivity::Failed(_)));
    }

    #[test]
    fn reading_shows_the_per_entry_bar_and_other_phases_show_a_spinner() {
        let mut screen = RunScreen::launching(plaintext_plan());
        screen.apply(RunMessage::Phase(Phase::Scanning));
        screen.apply(RunMessage::Entry { done: 6, total: 12 });
        let lines = rendered(&screen, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("6 / 12 Entries")),
            "expected the per-Entry Reading bar, got: {lines:?}"
        );

        screen.apply(RunMessage::Phase(Phase::Compressing));
        let lines = rendered(&screen, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("Compressing…")),
            "expected the indeterminate spinner line, got: {lines:?}"
        );
    }

    #[test]
    fn success_summary_shows_package_counts_and_the_vanished_callout_only_when_present() {
        let mut screen = RunScreen::launching(plaintext_plan());
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
        let outcome = BackupOutcome {
            backup_dir: PathBuf::from("/run/media/tester/USB/linux-backup-thinkpad-0"),
            manifest: manifest_with(
                3,
                vec![VanishedEntry {
                    path: "home/.cache/x".to_string(),
                    sensitive: false,
                }],
                packages,
            ),
        };
        screen.apply(RunMessage::Done(Box::new(outcome)));
        let lines = rendered(&screen, 70, 16);
        assert!(lines.iter().any(|l| l.contains("pacman 1")));
        assert!(lines.iter().any(|l| l.contains("1 Entries vanished")));
    }

    #[test]
    fn encrypted_success_summary_tags_the_run_encrypted() {
        let mut screen = RunScreen::launching(encrypted_plan());
        let outcome = BackupOutcome {
            backup_dir: PathBuf::from("/run/media/tester/USB/linux-backup-thinkpad-0"),
            manifest: manifest_with(2, Vec::new(), BTreeMap::new()),
        };
        screen.apply(RunMessage::Done(Box::new(outcome)));
        let lines = rendered(&screen, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("· encrypted")),
            "expected the encrypted tag, got: {lines:?}"
        );
    }
}
