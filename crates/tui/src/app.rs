//! The single `App` reducer and its pure render, per ADR-0005.
//!
//! One `App` struct in `crates/tui` holds all state; a [`Screen`] enum is its entry route and
//! [`View`] its live content, and behaviour is a pure `update(state, Event) -> state` reducer
//! plus a pure render function. That split is what makes event logic unit-testable with
//! hand-built `KeyEvent`s and rendering assertable via `ratatui::backend::TestBackend` —
//! neither needs a real terminal.
//!
//! This build (#67) is the wizard *shell*: the four-step completion-frontier stepper over a
//! persistent [`WizardState`] (module [`crate::wizard`]), the fixed header/footer frame that
//! doesn't jump on step change, the reflow ladder (module [`crate::reflow`]), and the
//! redraw-on-event loop's step-motion/esc/quit wiring. The WHERE step *body* (the location
//! pick-list + custom-path autocomplete, module [`crate::where_step`]) lands here in build #70;
//! the other step bodies (WHAT, encryption, REVIEW) and the restore/RUN screens land in the
//! sibling builds (#68/#69/#71/#72), so those remain placeholders.

use std::collections::BTreeMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::model::SelectionModel;
use linux_backup_core::{crypto, scan, RunContext};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::cli::RestoreArgs;
use crate::glyphs::Glyphs;
use crate::reflow::ReflowTier;
use crate::restore_screen::{self, RestoreFlow};
use crate::run_screen::{BackupRequest, RunMessage, RunPlan, RunScreen};
use crate::where_step::WhereStep;
use crate::wizard::{StepStatus, Wizard, WizardState, WizardStep};

/// The entry route `main` seeds from the parsed CLI command. `backup` opens the wizard; the
/// others open their single screen (their bodies land in later builds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The backup wizard (WHAT → encryption → WHERE → REVIEW), seeded by `linux-backup backup`.
    Backup,
    /// The standalone WHAT screen, seeded by `linux-backup select`.
    Select,
    /// The standalone WHERE screen, seeded by `linux-backup where`.
    Where,
}

impl Screen {
    /// The short mode label a non-wizard screen shows in place of app-name chrome (#50: no
    /// app name anywhere; the stepper is the wizard's header, a mode label everything else's).
    fn mode_label(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Select => "select",
            Self::Where => "where",
        }
    }
}

/// The live content behind the frame: the backup wizard, the RUN screen the wizard launches
/// into, the restore flow, or a not-yet-built screen's placeholder. Later builds replace the
/// remaining placeholders with the dev screens. Not `Clone`: the restore flow owns a live
/// worker channel (a `Receiver`), which has no meaningful copy — the reducer moves state by
/// value rather than cloning it.
#[derive(Debug)]
enum View {
    Wizard(Wizard),
    Run(RunScreen),
    Restore(RestoreFlow),
    Placeholder(Screen),
}

/// Everything the reducer consumes. The shared runtime (`runtime`) translates crossterm's
/// input into these — key presses (already filtered to `KeyEventKind::Press`) and terminal
/// resizes — so `update` never touches the terminal or crossterm's read loop directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
    /// A poll tick, emitted by the runtime only while a screen is polling: the backup/restore
    /// RUN (ADR-0005 D4), or the wizard's WHAT step while one of its own probes is pending
    /// (ADR-0009 D2). It carries no data — it just gives the active flow a chance to drain its
    /// worker channel and repaint. WHAT is *conditionally* polling — a `Tick` reaches it only
    /// while `wizard.what().has_pending_probe()` holds (see [`App::needs_poll`]); every other
    /// non-polling screen, and WHAT outside a pending probe, never sees one.
    Tick,
}

/// The whole application state: the active [`View`], the injected [`RunContext`] (ADR-0004:
/// run facts are injected, never read ambiently by anything under `core`), the quit flag the
/// runtime watches, and a one-shot backup-launch request the runtime drains to start the RUN
/// worker.
///
/// Not `Clone`: [`View::Restore`] owns a live worker channel that cannot be meaningfully
/// copied; the reducer moves `self` by value rather than cloning it.
#[derive(Debug)]
pub struct App {
    view: View,
    /// The WHERE step's view state (build #70). It lives in `App` around the wizard's model
    /// (ADR-0005) because it needs the injected [`RunContext`] (`home`, `privilege`) and the
    /// env-var resolver the wizard reducer doesn't carry; only the Backup route populates it.
    where_step: WhereStep,
    run_context: RunContext,
    should_quit: bool,
    /// Set once when REVIEW launches a backup; the runtime takes it to spawn the worker thread
    /// (the reducer stays pure — it records the request, it doesn't touch a thread). `None`
    /// whenever no launch is pending.
    pending_backup: Option<BackupRequest>,
    /// The view-only passphrase-reveal flag `F2` toggles (#51). It lives here, around the
    /// model, and is read only by the encryption body's render — the passphrase itself stays a
    /// `secrecy::SecretString`; reveal formats `expose_secret()` for one frame and retains
    /// nothing, so this bool is the whole of what a reveal persists. The restore flow keeps its
    /// own per-unlock reveal, so this covers the wizard's encryption step only.
    reveal: bool,
    /// Whether the `F1` help overlay is open (#51/#77). A view-only flag, like `reveal`: while
    /// set, the overlay captures input (`Esc`/`F1` close, `↑`/`↓` scroll) and every screen renders
    /// it on top — at any terminal size, even below the reflow ladder's Refuse cutoff.
    help_open: bool,
    /// The help overlay's scroll offset, so a short terminal can page a panel taller than it.
    help_scroll: u16,
    /// The no-emoji fallback glyph set, decided once at startup from the locale (module
    /// [`crate::glyphs`]) and threaded into every render — the ambient-run-fact end of ADR-0004.
    glyphs: Glyphs,
}

impl App {
    /// Builds the initial state for `screen`, carrying the machine's `run_context`. The
    /// `backup` route seeds the wizard from a Smart-default Selection (a `core` scan over the
    /// injected context) and pre-fills WHERE with the context's Output location, so the shell
    /// opens on a populated wizard rather than an empty one.
    #[must_use]
    pub fn new(screen: Screen, run_context: RunContext, glyphs: Glyphs) -> Self {
        // Only the wizard shows a WHERE body; the other routes get an inert step that probes no
        // hardware (they are separate invocations of the tool).
        let where_step = match screen {
            Screen::Backup => WhereStep::from_context(&run_context),
            _ => WhereStep::empty(),
        };
        let view = match screen {
            Screen::Backup => {
                let mut wizard = seed_wizard(&run_context);
                // Reconcile the wizard's optimistic seed with the WHERE screen's real
                // parent-writable probe of the pre-filled Output location (build #70).
                let (location, writable) = where_step.chosen();
                wizard.set_where(location, writable);
                View::Wizard(wizard)
            }
            other => View::Placeholder(other),
        };
        Self {
            view,
            where_step,
            run_context,
            should_quit: false,
            pending_backup: None,
            reveal: false,
            help_open: false,
            help_scroll: 0,
            glyphs,
        }
    }

    /// Builds the initial state for the `restore <dir>` route (#72). Unlike the wizard, restore
    /// is seeded from a CLI argument (the Backup dir) rather than a scan, so it has its own
    /// constructor: the [`RestoreFlow`] probes the dir (via `core`, ADR-0005 D6) to decide
    /// whether to open on the overwrite-confirm, an encrypted-Backup unlock path, or a fatal
    /// report for an invalid dir.
    #[must_use]
    pub fn new_restore(args: RestoreArgs, run_context: RunContext, glyphs: Glyphs) -> Self {
        let flow = RestoreFlow::new(
            args.backup_dir,
            run_context.clone(),
            args.verify,
            args.overwrite,
        );
        Self {
            view: View::Restore(flow),
            // Restore never enters the wizard, so its WHERE body is inert and no backup launch
            // is ever pending; its F2 reveal lives inside the flow's unlock stage, so the
            // wizard-only `App::reveal` stays false here.
            where_step: WhereStep::empty(),
            run_context,
            should_quit: false,
            pending_backup: None,
            reveal: false,
            help_open: false,
            help_scroll: 0,
            glyphs,
        }
    }

    /// Whether the runtime should tear down and exit after the last `update`.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Takes the pending backup-launch request, if REVIEW just launched one. The runtime calls
    /// this each loop turn and, on `Some`, spawns the worker thread that drives `core::backup`
    /// over the RUN progress channel. One-shot: it yields the request once, then clears.
    pub fn take_backup_request(&mut self) -> Option<BackupRequest> {
        self.pending_backup.take()
    }

    /// Whether a backup is in flight — while this holds the runtime polls the RUN worker's
    /// progress channel + crossterm each tick (rather than blocking on input) and drives an
    /// [`Event::Tick`] to animate the spinner. See also [`App::needs_poll`] for restore.
    #[must_use]
    pub fn is_backup_running(&self) -> bool {
        matches!(&self.view, View::Run(run) if run.is_running())
    }

    /// Folds one worker [`RunMessage`] into the RUN screen (phase/entry progress, or the
    /// terminal outcome). A no-op off the RUN screen.
    pub fn apply_run_message(&mut self, message: RunMessage) {
        if let View::Run(run) = &mut self.view {
            run.apply(message);
        }
    }

    /// The worker channel disconnected without a terminal message (a worker panic) — mark the
    /// RUN as a failed run. A no-op off the RUN screen.
    pub fn on_worker_disconnected(&mut self) {
        if let View::Run(run) = &mut self.view {
            run.on_worker_disconnected();
        }
    }

    /// Whether the current screen wants the runtime to poll rather than block on input — true
    /// while the restore worker is running (ADR-0005 D4), or while the wizard's WHAT step has a
    /// probe of its own in flight (ADR-0009 D2). The backup RUN screen polls too, but its worker
    /// lives in the runtime, so it is gated by [`App::is_backup_running`] instead; the runtime
    /// polls whenever any of the three holds. When polling, the runtime drives an [`Event::Tick`]
    /// each interval so the running flow can drain its worker channel.
    #[must_use]
    pub fn needs_poll(&self) -> bool {
        matches!(&self.view, View::Restore(flow) if flow.is_running())
            || matches!(&self.view, View::Wizard(wizard) if wizard.what().has_pending_probe())
    }

    /// The reducer: fold one [`Event`] into the next state. Taking and returning `Self` by value
    /// keeps it a total function of its inputs — the property the reducer unit tests rely on —
    /// with no `&mut` aliasing. The single impurity is the restore launch (a Confirm/Unlock
    /// `Enter` routed into the flow spawns the worker thread, ADR-0005 D4), mirroring the RUN
    /// screen; every other transition is side-effect-free.
    ///
    /// `Ctrl-C` is the sole quit (the #51 keymap) and is handled here, before any view sees
    /// the key — on a running backup/restore it doubles as the abort. Every other key routes to
    /// the active view; `Esc` there means back/close and never quits. [`Event::Tick`] is
    /// delivered only while a screen is polling and advances whichever flow is running (the RUN
    /// spinner, the restore flow's channel drain, or WHAT's own pending-probe drain, ADR-0009
    /// D2) — it never blocks a key from reaching the wizard's reducer in the same turn.
    /// Resizes are folded so the loop repaints; the reflow tier is recomputed from the frame at
    /// render time, so there is no size-dependent state to store here — the one deliberate
    /// exception is the WHAT/WHERE custom-path fields' `visible_cap` (#87), cached by `render`
    /// itself since it alone knows the post-layout inner height (see
    /// [`crate::where_step::WhereStep`]'s `visible_cap` doc).
    #[must_use]
    pub fn update(mut self, event: Event) -> Self {
        // A poll tick advances whichever flow is running: the RUN spinner (so an indeterminate
        // phase still animates on a tick with no worker message), the restore flow (draining its
        // own worker channel), or the wizard's WHAT step (draining its own pending probe,
        // ADR-0009 D2, regardless of which step is currently active — see `Wizard::tick_what`).
        // It reaches the reducer only while a screen is polling.
        if let Event::Tick = event {
            self.view = match self.view {
                View::Run(mut run) => {
                    run.tick();
                    View::Run(run)
                }
                View::Restore(flow) => View::Restore(flow.tick()),
                View::Wizard(mut wizard) => {
                    wizard.tick_what();
                    View::Wizard(wizard)
                }
                View::Placeholder(screen) => View::Placeholder(screen),
            };
            return self;
        }
        let Event::Key(key) = event else {
            // Resize (and any other non-key event): nothing to fold — the loop repaints and the
            // reflow tier is recomputed from the frame at render time.
            return self;
        };
        // `Ctrl-C` is the sole quit everywhere, and during a running backup/restore it *is* the
        // cancel (the run is abandoned as the app tears down — `core` is synchronous with no
        // cooperative cancellation, so aborting the process is the honest cancel here). Handled
        // before the help overlay so it aborts even with the overlay open.
        if crate::keymap::CTRL_C.matches(&key) {
            self.should_quit = true;
            return self;
        }
        // While the `F1` overlay is open it is captive (#51): `Esc`/`F1` close it, `↑`/`↓` (and the
        // page/Home/End keys) scroll it, and every other key is swallowed so nothing behind it
        // acts. Handled before the rest of the keymap so no chord leaks through to a screen.
        if self.help_open {
            self.handle_help_key(key);
            return self;
        }
        // The terminal-reserved chords are never bound (#51): swallow them everywhere so no
        // screen can act on one, matching the catalog's reserved set rather than restating it
        // (#74). `Ctrl-C` (the sole honored control chord) is handled above.
        if crate::keymap::is_reserved(&key) {
            return self;
        }
        // `F1` opens the exhaustive help overlay from anywhere (#51 Global / #77). It rides an
        // Fn key, not the plain `?` the original keymap used, so it stays live inside a focused
        // text field — a plain `?` now belongs to the focused widget and is typed as a literal
        // character (an `age` passphrase may contain one, #77), never intercepted here.
        if key.code == KeyCode::F(1) {
            self.help_open = true;
            self.help_scroll = 0;
            return self;
        }
        // F2 is the passphrase-reveal chord (#51): a view-only toggle handled here so it stays
        // live regardless of which field owns plain keys, and never reaches the reducer as text.
        // The restore flow keeps its own per-unlock reveal, so F2 is left to fall through to it.
        if key.code == KeyCode::F(2) && !matches!(self.view, View::Restore(_)) {
            self.reveal = !self.reveal;
            return self;
        }
        // F3 toggles hidden-file visibility in an autocomplete field (#51 Reveal). On the WHAT
        // step with the Custom-path field open it flips *that* field's toggle; otherwise it flips
        // the WHERE field's (which lives on `App`, and is a harmless no-op on the inert routes).
        if key.code == KeyCode::F(3) {
            if let View::Wizard(wizard) = &mut self.view
                && wizard.current() == WizardStep::What
                && wizard.what().is_typing()
            {
                wizard.toggle_what_show_hidden();
                return self;
            }
            self.where_step.toggle_show_hidden();
            return self;
        }
        match self.view {
            // Plain `Enter` on REVIEW launches the backup (#39: not `Ctrl-Enter`, which some
            // terminals swallow); any other key drives the wizard.
            View::Wizard(ref wizard)
                if wizard.current() == WizardStep::Review
                    && key.code == KeyCode::Enter
                    && key.modifiers.is_empty() =>
            {
                self.launch_backup();
            }
            View::Wizard(mut wizard) => {
                // On the WHERE step the screen body owns the plain keys (typing, the candidate
                // list, Tab/→/Enter); the global step-motion chords stay with the wizard (#51).
                // The screen needs the injected run facts + env resolver, so it is driven here
                // in `App` rather than inside the wizard reducer (ADR-0004/0005 D6).
                if wizard.current() == WizardStep::Where
                    && where_key_targets_screen(key, self.where_step.is_typing())
                {
                    self.where_step
                        .handle_key(key, &|name| std::env::var(name).ok());
                    let (location, writable) = self.where_step.chosen();
                    wizard.set_where(location, writable);
                    self.view = View::Wizard(wizard);
                } else if wizard.current() == WizardStep::What
                    && wizard.what().is_typing()
                    && what_field_targets_screen(key)
                {
                    // The WHAT Custom-path field owns the plain keys while it is open; like WHERE
                    // it needs the injected run facts + env resolver, so it is driven here rather
                    // than inside the pure wizard reducer (ADR-0004/0005 D6).
                    wizard.handle_what_field_key(
                        key,
                        &self.run_context.home,
                        self.run_context.privilege,
                        &|name| std::env::var(name).ok(),
                    );
                    self.view = View::Wizard(wizard);
                } else {
                    // A `→`/`Enter` on the Custom-paths row, a bare `a` from anywhere on WHAT, or
                    // a Category-emptying `r` (#106/#107/#148) can each newly open the WHAT
                    // Custom-path field from inside the pure reducer, so the privilege-aware
                    // `~/`/`/` prefill (#85) is primed right after every key on this path.
                    // `prime_*` self-guards on an empty buffer: it fills the field the frame it
                    // opens and is otherwise a no-op — including when a step-motion chord (which
                    // also re-enters this branch while the field is open) must not clobber what
                    // the user has typed.
                    let mut wizard = wizard.handle_key(key);
                    wizard.prime_what_custom_path(self.run_context.privilege);
                    self.view = View::Wizard(wizard);
                }
            }
            // On a finished run, plain `Enter` closes the app (#61's `Enter close`); while it
            // runs, only `Ctrl-C` (handled above) does anything.
            View::Run(ref run)
                if run.is_terminal() && key.code == KeyCode::Enter && key.modifiers.is_empty() =>
            {
                self.should_quit = true;
            }
            // The restore flow owns its keys (overwrite-confirm, passphrase unlock, dismissing
            // the result); a Confirm/Unlock `Enter` routed in spawns its worker thread.
            View::Restore(flow) => {
                self.view = View::Restore(flow.handle_key(key));
                // The standalone restore route has nothing behind it, so closing its flow
                // (Cancel, or dismissing the result) is a quit — folded here rather than
                // reaching into `should_quit` from inside the flow.
                if let View::Restore(flow) = &self.view
                    && flow.wants_exit()
                {
                    self.should_quit = true;
                }
            }
            // A running RUN ignores everything but Ctrl-C (above); placeholder screens have no
            // interactions yet (`Esc` is inert, never quits).
            View::Run(_) | View::Placeholder(_) => {}
        }
        self
    }

    /// Folds one key while the `F1` overlay is open (#51/#77). `Esc`/`F1` close it; the movement
    /// keys scroll it so a panel taller than the terminal is fully reachable; everything else is
    /// swallowed (the overlay is captive). Scroll is clamped to the binding count so paging past
    /// the end can't scroll the content out of view.
    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::F(1) => {
                self.help_open = false;
                self.help_scroll = 0;
            }
            KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Down => {
                self.help_scroll = self
                    .help_scroll
                    .saturating_add(1)
                    .min(crate::help::line_count());
            }
            KeyCode::PageUp | KeyCode::Home => self.help_scroll = 0,
            KeyCode::PageDown | KeyCode::End => self.help_scroll = crate::help::line_count(),
            _ => {}
        }
    }

    /// Transitions from REVIEW into the RUN screen and records the launch request the runtime
    /// drains to start the worker. The Selection, encryption choice, and Output location are
    /// cloned out of the wizard here so the worker owns self-contained inputs; the context's
    /// `output_location` is pointed at the chosen Backup location (mirroring the alpha's
    /// `ctx.output_location = location` before `core::backup`).
    fn launch_backup(&mut self) {
        let View::Wizard(wizard) = &self.view else {
            return;
        };
        let state = wizard.state();
        let mut ctx = self.run_context.clone();
        if let Some(location) = state.location() {
            ctx.output_location = location.to_path_buf();
        }
        let selection = state.selection().clone();
        let encryption = state.encryption().clone();
        let plan = RunPlan::new(&selection, &encryption, &ctx);
        self.pending_backup = Some(BackupRequest {
            selection,
            ctx,
            encryption,
        });
        self.view = View::Run(RunScreen::launching(plan));
    }

    /// The pure render: draw the current state into `frame`. Below the reflow ladder's Refuse
    /// cutoff every view collapses to a single full-screen "terminal too small" notice; above
    /// it, the wizard draws its fixed frame and the placeholders a titled mode block.
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let tier = ReflowTier::for_height(area.height);
        if tier.renders_frame() {
            match &self.view {
                View::Wizard(wizard) => {
                    render_wizard(
                        frame,
                        area,
                        wizard,
                        &self.where_step,
                        tier,
                        &self.run_context,
                        self.reveal,
                        self.glyphs,
                    );
                }
                View::Run(run) => crate::run_screen::render(frame, area, run, tier, self.glyphs),
                View::Restore(flow) => {
                    restore_screen::render(frame, area, flow, tier, self.glyphs);
                }
                View::Placeholder(screen) => render_placeholder(frame, area, *screen),
            }
        } else {
            render_too_small(frame, area);
        }
        // The overlay draws last, on top of whatever the base render produced — including the
        // sub-Refuse too-small notice, so `?` help is reachable at any terminal size (#51).
        if self.help_open {
            crate::help::render(frame, area, self.help_scroll);
        }
    }
}

/// Whether a key pressed on the WHERE step belongs to the screen body rather than the wizard's
/// global navigation (#51). The step-motion chords (`Ctrl-←`/`Ctrl-→`) always stay with the
/// wizard; `1`–`4` jump stays with the wizard *unless* the custom-path field is focused, where a
/// typed digit must reach the field; `Esc` closes the field first while typing, else steps back.
/// Everything else — typing, the candidate list, `Tab`/`→`/`Enter` — is the screen's.
fn where_key_targets_screen(key: KeyEvent, typing: bool) -> bool {
    if crate::keymap::CTRL_LEFT.matches(&key) || crate::keymap::CTRL_RIGHT.matches(&key) {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if !ctrl && WizardStep::from_digit(c).is_some() => typing,
        KeyCode::Esc => typing,
        _ => true,
    }
}

/// Whether a key pressed while the WHAT Custom-path field is open belongs to the field rather
/// than the wizard (#51/#75). Only called when the field is focused: the step-motion chords
/// (`Ctrl-←`/`Ctrl-→`) stay with the wizard so they move between steps even mid-entry, while
/// everything else — typing, the candidate list, `Tab`/`→`/`Enter`, `Esc` (which closes the
/// field), and a typed `1`–`4` digit — is the field's.
fn what_field_targets_screen(key: KeyEvent) -> bool {
    !(crate::keymap::CTRL_LEFT.matches(&key) || crate::keymap::CTRL_RIGHT.matches(&key))
}

/// Seeds a wizard from a Smart-default Selection and the context's Output location. This is
/// the one place the shell reaches into `core`'s scan; the reducer and render stay pure over
/// the resulting [`WizardState`].
fn seed_wizard(ctx: &RunContext) -> Wizard {
    let scanned = scan::scan_taxonomy(ctx);
    let selection = SelectionModel::from_smart_defaults(&scanned);
    Wizard::new(WizardState::new(
        selection,
        Some(ctx.output_location.clone()),
    ))
}

/// The sub-`Refuse` notice: the frame can't hold, so draw one centered message rather than a
/// broken frame (#50).
fn render_too_small(frame: &mut Frame, area: Rect) {
    let notice = Paragraph::new(format!(
        "Terminal too short — needs ≥ {} rows.",
        crate::reflow::MIN_FRAME_ROWS
    ))
    .alignment(Alignment::Center);
    frame.render_widget(notice, area);
}

/// A not-yet-built screen: a titled block carrying its short mode label (#50) so the shared
/// runtime has something to drive until the real screen lands.
fn render_placeholder(frame: &mut Frame, area: Rect, screen: Screen) {
    let block = Paragraph::new(format!(
        "{} — screen body lands in a later build",
        screen.mode_label()
    ))
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(screen.mode_label()),
    );
    frame.render_widget(block, area);
}

/// Draws the wizard's fixed frame: header (stepper, plus a context line at Comfortable), an
/// optional gate banner, the body region, and the footer hints. Only the body swaps between
/// steps — the header and footer keep their rows, so the frame never jumps on step change
/// (ADR-0007 D6).
// The eight inputs are the frame's irreducible render facts (state, layout tier, the injected
// context, and the two view-only flags reveal/glyphs); bundling them into a struct would only
// move the argument list, not shorten what the frame legitimately depends on.
#[allow(clippy::too_many_arguments)]
fn render_wizard(
    frame: &mut Frame,
    area: Rect,
    wizard: &Wizard,
    where_step: &WhereStep,
    tier: ReflowTier,
    ctx: &RunContext,
    reveal: bool,
    glyphs: Glyphs,
) {
    let banner_rows = u16::from(wizard.gate_banner().is_some());
    let chunks = Layout::vertical([
        Constraint::Length(tier.header_rows()),
        Constraint::Length(banner_rows),
        Constraint::Min(crate::reflow::MIN_BODY_ROWS),
        Constraint::Length(tier.footer_rows()),
    ])
    .split(area);

    render_header(frame, chunks[0], wizard, tier, glyphs);
    if banner_rows > 0 {
        render_gate_banner(frame, chunks[1], wizard);
    }
    render_body(frame, chunks[2], wizard, where_step, ctx, reveal, glyphs);
    render_footer(frame, chunks[3], wizard, tier);
}

/// The fixed header: the stepper row, plus the context/status line beneath it at Comfortable.
fn render_header(frame: &mut Frame, area: Rect, wizard: &Wizard, tier: ReflowTier, glyphs: Glyphs) {
    if tier.shows_context_line() && area.height >= 2 {
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
        frame.render_widget(Paragraph::new(stepper_line(wizard, tier, glyphs)), rows[0]);
        frame.render_widget(
            Paragraph::new(context_line(wizard))
                .style(Style::default().add_modifier(Modifier::DIM)),
            rows[1],
        );
    } else {
        frame.render_widget(Paragraph::new(stepper_line(wizard, tier, glyphs)), area);
    }
}

/// The stepper glyph for a step status in the active glyph set (#50/#61). The invalidated `!`
/// is already ASCII, so it is fixed; the rest come from the set.
fn status_glyph(status: StepStatus, glyphs: Glyphs) -> &'static str {
    match status {
        StepStatus::Completed => glyphs.check,
        StepStatus::Current => glyphs.current,
        StepStatus::Locked => glyphs.locked,
        StepStatus::Invalidated => "!",
    }
}

/// The horizontal stepper (#50). Full form spells every step (`glyph Title → …`); the compact
/// form (Tight and below) shows the glyphs plus only the current step's title. The current
/// step's title is marked by color/contrast (not the glyph alone), a Sensitive Selection adds
/// a lock to the Encryption label, and an invalidated encryption step is yellow.
fn stepper_line(wizard: &Wizard, tier: ReflowTier, glyphs: Glyphs) -> Line<'static> {
    let sensitive = wizard.state().is_sensitive();
    if tier.compact_stepper() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for step in WizardStep::ALL {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(status_glyph(wizard.step_status(step), glyphs)));
        }
        // The `·` here is a separator between the glyphs and the current title, so it stays
        // Unicode in both sets (#61: only the semantic `·` glyph remaps).
        spans.push(Span::raw("  ·  "));
        spans.push(step_title_span(wizard, wizard.current(), sensitive, glyphs));
        return Line::from(spans);
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, step) in WizardStep::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  →  "));
        }
        let status = wizard.step_status(step);
        spans.push(Span::raw(format!("{} ", status_glyph(status, glyphs))));
        spans.push(step_title_span(wizard, step, sensitive, glyphs));
    }
    Line::from(spans)
}

/// One step's title span, styled by its stepper status (#50). The lock suffix rides the
/// Encryption title when the Selection is Sensitive.
fn step_title_span(
    wizard: &Wizard,
    step: WizardStep,
    sensitive: bool,
    glyphs: Glyphs,
) -> Span<'static> {
    let mut title = step.title().to_string();
    if step == WizardStep::Encryption && sensitive {
        title.push(' ');
        title.push_str(glyphs.lock);
    }
    let style = match wizard.step_status(step) {
        StepStatus::Current => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        StepStatus::Invalidated => Style::default().fg(Color::Yellow),
        StepStatus::Completed => Style::default(),
        StepStatus::Locked => Style::default().add_modifier(Modifier::DIM),
    };
    Span::styled(title, style)
}

/// The Comfortable context/status line — the pure facts the shell can show without touching
/// the filesystem (size and the restored/pruned counts are screen-body concerns for later
/// builds).
fn context_line(wizard: &Wizard) -> String {
    let selection = wizard.state().selection();
    let entries = selection.entries().len();
    let mut categories: Vec<&str> = selection.entries().iter().map(|e| e.category).collect();
    categories.sort_unstable();
    categories.dedup();
    format!("{entries} selected · {} categories", categories.len())
}

/// The gate-reason banner: the verbatim reason a forward move was refused, centered in its own
/// row and dismissed on the next input (no timer, #61).
fn render_gate_banner(frame: &mut Frame, area: Rect, wizard: &Wizard) {
    let Some(reason) = wizard.gate_banner() else {
        return;
    };
    let banner = Paragraph::new(reason.message())
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(banner, area);
}

/// The body region — the only part that swaps between steps. WHAT is the two-pane explorer
/// (build #68), the encryption body (build #69), the WHERE pick-list + custom-path autocomplete
/// (build #70), and the REVIEW summary (build #71) all render here; any remaining step stays a
/// placeholder until its build (#72).
fn render_body(
    frame: &mut Frame,
    area: Rect,
    wizard: &Wizard,
    where_step: &WhereStep,
    ctx: &RunContext,
    reveal: bool,
    glyphs: Glyphs,
) {
    if wizard.current() == WizardStep::What {
        crate::what_screen::render(
            frame,
            area,
            wizard.what(),
            wizard.state(),
            &ctx.home,
            ctx.privilege,
            &|name| std::env::var(name).ok(),
            glyphs,
        );
        return;
    }
    if wizard.current() == WizardStep::Encryption {
        crate::encryption_screen::render(frame, area, wizard, reveal, glyphs);
        return;
    }
    if wizard.current() == WizardStep::Where {
        // The selection's uncompressed size feeds the advisory space warning only; it never
        // gates (#61). Read it here (a `core` probe) rather than in the reducer.
        let selection_size = wizard.state().selection().total_size();
        where_step.render(
            frame,
            area,
            selection_size,
            &|name| std::env::var(name).ok(),
            glyphs,
        );
        return;
    }
    if wizard.current() == WizardStep::Review {
        frame.render_widget(
            Paragraph::new(review_lines(wizard.state(), ctx, glyphs)),
            area,
        );
        return;
    }
    let body = format!(
        "{} — screen body lands in a later build",
        wizard.current().title()
    );
    frame.render_widget(Paragraph::new(body).alignment(Alignment::Center), area);
}

/// The REVIEW summary (#61 copy): what will be backed up, where, and the encryption state —
/// everything chosen, summarized before anything is written. The Backup dir is computed from
/// the same `ctx` `core::backup` will use, so what REVIEW shows is exactly where the run lands.
fn review_lines(state: &WizardState, ctx: &RunContext, glyphs: Glyphs) -> Vec<Line<'static>> {
    let selection = state.selection();
    let mut categories: BTreeMap<&'static str, usize> = BTreeMap::new();
    for entry in selection.entries() {
        *categories.entry(entry.category).or_insert(0) += 1;
    }
    let breakdown = categories
        .iter()
        .map(|(category, count)| format!("{category} {count}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let location = state
        .location()
        .map_or_else(|| ctx.output_location.clone(), Path::to_path_buf);
    let backup_dir = location.join(linux_backup_core::backup_dir_name(ctx));

    vec![
        Line::from("REVIEW — nothing is written until you confirm"),
        Line::default(),
        Line::from(format!(
            "Selection   {} Entries · {} Categories · {}",
            selection.entries().len(),
            categories.len(),
            format_size(selection.total_size()),
        )),
        Line::from(format!("   {breakdown}")),
        Line::from(format!("Output      {}", location.display())),
        Line::from(format!("Backup dir  {}", backup_dir.display())),
        encryption_review_line(state, glyphs),
        Line::default(),
        Line::from("(To change anything, press 1–4 to jump back to a step.)"),
        Line::default(),
        Line::from(Span::styled(
            crate::keymap::START_BACKUP.label(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
    ]
}

/// The REVIEW encryption line's three #61 states: encrypted, plaintext with nothing Sensitive,
/// or plaintext shipping Sensitive content anyway (named, warned).
fn encryption_review_line(state: &WizardState, glyphs: Glyphs) -> Line<'static> {
    if state.encryption().is_encrypted() {
        return Line::from("Encryption  encrypted with a passphrase");
    }
    let sensitive_categories = crypto::evaluate(state.selection()).sensitive_categories;
    if sensitive_categories.is_empty() {
        Line::from("Encryption  not encrypted")
    } else {
        Line::from(Span::styled(
            format!(
                "Encryption  {} not encrypted — {} in plaintext",
                glyphs.warn,
                sensitive_categories.join(", ")
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
    }
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based), matching the other screens'
/// size rendering.
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

/// The fixed footer hints, degrading down the reflow ladder but always keeping `F1 keys` (#50).
/// Every label comes from the keymap catalog (#74) — the footer never spells a chord itself.
fn render_footer(frame: &mut Frame, area: Rect, wizard: &Wizard, tier: ReflowTier) {
    // REVIEW is terminal — the forward action is launching the run, so its footer leads with
    // `Enter start backup` rather than the `Ctrl-→ next` the earlier steps show (#61).
    let hints = if wizard.current() == WizardStep::Review {
        crate::keymap::wizard_review_footer(tier)
    } else if wizard.current() == WizardStep::What && !wizard.what().is_typing() {
        // The WHAT step's own footer (#106/#107/#148) adds `a add · r remove` to the standard
        // hints — but only while the Custom-path field is closed; while it's open, the field
        // owns plain keys and `a`/`r` are inert, so the generic step footer stands instead.
        crate::keymap::what_step_footer(wizard.state().step_complete(wizard.current()), tier)
    } else {
        crate::keymap::wizard_step_footer(wizard.state().step_complete(wizard.current()), tier)
    };
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime};

    use crate::pending_probe::PendingProbe;
    use crate::what_screen::{ProbeTarget, WhatProbeResult};

    use linux_backup_core::model::{Entry, EntryRoot, EntryType};
    use linux_backup_core::Privilege;

    fn test_context() -> RunContext {
        RunContext {
            home: PathBuf::from("/home/tester"),
            output_location: PathBuf::from("/home/tester"),
            privilege: Privilege::NonRoot,
            host: "test-host".to_string(),
            user: "tester".to_string(),
            uid: 1000,
            os: "test-os".to_string(),
            now: SystemTime::UNIX_EPOCH,
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
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

    /// Builds an `App` on a hand-built wizard, bypassing the FS-scanning `seed_wizard` so
    /// render/reducer tests are deterministic and TTY-free.
    fn app_with_wizard(wizard: Wizard) -> App {
        App {
            view: View::Wizard(wizard),
            where_step: WhereStep::empty(),
            run_context: test_context(),
            should_quit: false,
            pending_backup: None,
            reveal: false,
            help_open: false,
            help_scroll: 0,
            glyphs: Glyphs::UNICODE,
        }
    }

    fn non_sensitive_wizard() -> Wizard {
        let selection = SelectionModel::new(vec![entry("Shell", false, ".zshrc")]);
        Wizard::new(WizardState::new(
            selection,
            Some(PathBuf::from("/home/tester")),
        ))
    }

    fn sensitive_wizard() -> Wizard {
        let selection = SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]);
        Wizard::new(WizardState::new(
            selection,
            Some(PathBuf::from("/home/tester")),
        ))
    }

    /// Collapses a rendered buffer into per-row strings so assertions can look for text
    /// without pinning exact cell styling.
    fn rendered_lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend terminal");
        terminal
            .draw(|frame| app.render(frame))
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
    fn ctrl_c_requests_quit() {
        let app = app_with_wizard(non_sensitive_wizard());
        assert!(!app.should_quit());
        let app = app.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());
    }

    #[test]
    fn plain_c_does_not_quit() {
        let app = app_with_wizard(non_sensitive_wizard());
        let app = app.update(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!app.should_quit());
    }

    #[test]
    fn esc_never_quits() {
        let app = app_with_wizard(non_sensitive_wizard());
        let app = app.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.should_quit());
        // And on a placeholder screen.
        let app = App::new(Screen::Select, test_context(), Glyphs::UNICODE);
        let app = app.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.should_quit());
    }

    #[test]
    fn resize_is_folded_without_quitting() {
        let app = app_with_wizard(non_sensitive_wizard());
        let app = app.update(Event::Resize(80, 24));
        assert!(!app.should_quit());
    }

    #[test]
    fn stepper_shows_current_and_locked_states() {
        let app = app_with_wizard(sensitive_wizard());
        let lines = rendered_lines(&app, 80, 24);
        let header = &lines[0];
        // Current WHAT (●), and a locked-ahead WHERE/Review (·) because the Sensitive
        // encryption step isn't decided.
        assert!(
            header.contains('●'),
            "expected a current-step glyph, got: {header:?}"
        );
        assert!(
            header.contains('·'),
            "expected a locked-step glyph, got: {header:?}"
        );
        assert!(
            header.contains("WHAT"),
            "expected step titles, got: {header:?}"
        );
    }

    #[test]
    fn sensitive_selection_adds_a_lock_to_the_encryption_label() {
        let app = app_with_wizard(sensitive_wizard());
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines[0].contains('🔒'),
            "expected a lock on the Encryption label, got: {:?}",
            lines[0]
        );
    }

    #[test]
    fn completed_steps_render_the_check_glyph() {
        // Non-Sensitive with a location: WHAT/encryption/WHERE all complete; stepping to
        // WHERE leaves two ✓ behind.
        let wizard = non_sensitive_wizard()
            .handle_key(KeyEvent::new_with_kind(
                KeyCode::Right,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))
            .handle_key(KeyEvent::new_with_kind(
                KeyCode::Right,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ));
        let app = app_with_wizard(wizard);
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines[0].contains('✓'),
            "expected completed glyphs, got: {:?}",
            lines[0]
        );
    }

    #[test]
    fn refused_forward_renders_the_gate_banner() {
        // Empty Selection → WHAT incomplete → Ctrl-→ refuses with the reason banner.
        let wizard = Wizard::new(WizardState::new(SelectionModel::new(vec![]), None));
        let app = app_with_wizard(wizard).update(key(KeyCode::Right, KeyModifiers::CONTROL));
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Nothing selected")),
            "expected the gate banner, got: {lines:?}"
        );
    }

    #[test]
    fn comfortable_tier_shows_the_context_line() {
        let app = app_with_wizard(non_sensitive_wizard());
        let lines = rendered_lines(&app, 80, 32);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("selected") && l.contains("categories")),
            "expected the context/status line at Comfortable, got: {lines:?}"
        );
    }

    #[test]
    fn default_tier_drops_the_context_line() {
        let app = app_with_wizard(non_sensitive_wizard());
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            !lines.iter().any(|l| l.contains("categories")),
            "context line should fold away below Comfortable, got: {lines:?}"
        );
    }

    #[test]
    fn refuse_tier_shows_only_the_too_small_notice() {
        let app = app_with_wizard(non_sensitive_wizard());
        let lines = rendered_lines(&app, 80, 8);
        assert!(
            lines.iter().any(|l| l.contains("Terminal too short")),
            "expected the too-small notice, got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("WHAT")),
            "the frame must not render below Refuse, got: {lines:?}"
        );
    }

    #[test]
    fn footer_always_keeps_the_help_hint_across_tiers() {
        for height in [32u16, 24, 16, 12] {
            let app = app_with_wizard(non_sensitive_wizard());
            let lines = rendered_lines(&app, 80, height);
            assert!(
                lines.iter().any(|l| l.contains("F1 keys")),
                "footer lost `F1 keys` at height {height}, got: {lines:?}"
            );
        }
    }

    /// A non-Sensitive wizard with a location, advanced all the way to the terminal REVIEW
    /// step — the launch preconditions.
    fn wizard_on_review() -> Wizard {
        non_sensitive_wizard().handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('4'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ))
    }

    #[test]
    fn enter_on_review_launches_the_backup() {
        let mut app = app_with_wizard(wizard_on_review());
        assert!(app.take_backup_request().is_none());
        let mut app = app.update(key(KeyCode::Enter, KeyModifiers::NONE));
        // The view is now the RUN screen with a backup in flight, and a launch request is
        // waiting for the runtime to spawn the worker.
        assert!(app.is_backup_running());
        assert!(!app.should_quit());
        assert!(app.take_backup_request().is_some());
        // One-shot: the request is drained, not handed out twice.
        assert!(app.take_backup_request().is_none());
    }

    #[test]
    fn enter_off_review_does_not_launch() {
        // On WHAT (not the terminal step), plain Enter is not a launch.
        let app =
            app_with_wizard(non_sensitive_wizard()).update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.is_backup_running());
    }

    #[test]
    fn enter_closes_a_finished_run() {
        let mut app =
            app_with_wizard(wizard_on_review()).update(key(KeyCode::Enter, KeyModifiers::NONE));
        let _ = app.take_backup_request();
        // Feed a terminal outcome as the worker would.
        app.apply_run_message(RunMessage::Failed {
            message: "boom".to_string(),
            cleaned_up: true,
        });
        assert!(!app.is_backup_running());
        // Enter on the finished run closes the app.
        let app = app.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.should_quit());
    }

    #[test]
    fn review_body_summarizes_the_run() {
        let app = app_with_wizard(wizard_on_review());
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("REVIEW")),
            "expected the REVIEW heading, got: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("Selection")));
        assert!(lines.iter().any(|l| l.contains("Output")));
        assert!(lines.iter().any(|l| l.contains("Backup dir")));
        // Non-Sensitive plaintext reads as "not encrypted", not the warning form.
        assert!(lines.iter().any(|l| l.contains("not encrypted")));
        // The REVIEW footer leads with the launch action.
        assert!(lines.iter().any(|l| l.contains("Enter start backup")));
    }

    #[test]
    fn review_warns_when_sensitive_content_ships_in_plaintext() {
        // A Sensitive Selection with a plaintext decision (Auto-insist disabled) sitting on
        // REVIEW: the encryption line names the exposed Category with the warning glyph.
        let mut state = WizardState::new(
            SelectionModel::new(vec![entry("SSH", true, "id_ed25519")]),
            Some(PathBuf::from("/home/tester")),
        );
        state.choose_plaintext();
        let wizard = Wizard::new(state).handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('4'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        let app = app_with_wizard(wizard);
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("not encrypted")
                && l.contains("SSH")
                && l.contains("plaintext")),
            "expected the plaintext-Sensitive warning, got: {lines:?}"
        );
    }

    /// Steps a non-Sensitive, located wizard forward to the WHERE step (WHAT and encryption both
    /// complete on arrival), so the WHERE body is what renders.
    fn wizard_on_where() -> Wizard {
        non_sensitive_wizard()
            .handle_key(KeyEvent::new_with_kind(
                KeyCode::Right,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))
            .handle_key(KeyEvent::new_with_kind(
                KeyCode::Right,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))
    }

    #[test]
    fn where_step_renders_its_body_not_the_placeholder() {
        let app = app_with_wizard(wizard_on_where());
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("WHERE to write the backup")),
            "expected the WHERE body, got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("lands in a later build")),
            "the WHERE step must render its real body, got: {lines:?}"
        );
    }

    #[test]
    fn where_routing_keeps_step_chords_with_the_wizard_and_body_keys_with_the_screen() {
        // Step-motion chords always route to the wizard, even on WHERE.
        assert!(!where_key_targets_screen(
            key_event(KeyCode::Right, KeyModifiers::CONTROL),
            true
        ));
        assert!(!where_key_targets_screen(
            key_event(KeyCode::Left, KeyModifiers::CONTROL),
            false
        ));
        // A digit jumps steps while picking, but reaches the field while typing (#51).
        assert!(!where_key_targets_screen(
            key_event(KeyCode::Char('3'), KeyModifiers::NONE),
            false
        ));
        assert!(where_key_targets_screen(
            key_event(KeyCode::Char('3'), KeyModifiers::NONE),
            true
        ));
        // Esc closes the field first while typing, else steps back through the wizard.
        assert!(where_key_targets_screen(
            key_event(KeyCode::Esc, KeyModifiers::NONE),
            true
        ));
        assert!(!where_key_targets_screen(
            key_event(KeyCode::Esc, KeyModifiers::NONE),
            false
        ));
        // Ordinary body keys always belong to the screen.
        assert!(where_key_targets_screen(
            key_event(KeyCode::Tab, KeyModifiers::NONE),
            false
        ));
        assert!(where_key_targets_screen(
            key_event(KeyCode::Char('a'), KeyModifiers::NONE),
            false
        ));
    }

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn restore_route_opens_on_the_overwrite_confirm() {
        // A non-existent dir opens on a fatal report rather than a confirm — but either way the
        // restore route renders its own screen, not the placeholder.
        let args = RestoreArgs {
            backup_dir: PathBuf::from("/does/not/exist"),
            verify: false,
            overwrite: false,
        };
        let app = App::new_restore(args, test_context(), Glyphs::UNICODE);
        let lines = rendered_lines(&app, 70, 16);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("failed") || l.contains("restore")),
            "expected a restore screen, got: {lines:?}"
        );
        assert!(
            !app.needs_poll(),
            "a freshly-seeded restore is not yet running"
        );
    }

    #[test]
    fn ctrl_c_quits_the_restore_route() {
        let args = RestoreArgs {
            backup_dir: PathBuf::from("/does/not/exist"),
            verify: false,
            overwrite: false,
        };
        let app = App::new_restore(args, test_context(), Glyphs::UNICODE);
        let app = app.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());
    }

    #[test]
    fn a_tick_on_a_non_polling_screen_is_inert() {
        let app = app_with_wizard(non_sensitive_wizard());
        let app = app.update(Event::Tick);
        assert!(!app.should_quit());
    }

    #[test]
    fn needs_poll_is_false_without_a_pending_what_probe() {
        let app = app_with_wizard(non_sensitive_wizard());
        assert!(!app.needs_poll());
    }

    #[test]
    fn needs_poll_is_true_while_what_has_a_pending_probe() {
        // ADR-0009 D2's third disjunct: WHAT joins the poll loop only while one of its own
        // probes is pending, alongside the existing restore/backup cases.
        let mut wizard = non_sensitive_wizard();
        wizard.what_mut().set_pending_probe(
            PendingProbe::spawn(|| WhatProbeResult::TotalSize(0)),
            ProbeTarget::Category("KDE Plasma"),
        );
        let app = app_with_wizard(wizard);
        assert!(app.needs_poll());
    }

    #[test]
    fn tick_drains_whats_pending_probe_and_needs_poll_goes_false_again() {
        let mut wizard = non_sensitive_wizard();
        wizard.what_mut().set_pending_probe(
            PendingProbe::spawn(|| WhatProbeResult::TotalSize(7)),
            ProbeTarget::Category("KDE Plasma"),
        );
        let mut app = app_with_wizard(wizard);
        assert!(
            app.needs_poll(),
            "expected polling while a probe is pending"
        );

        // Tick until the worker thread's result actually lands or a bounded deadline passes —
        // avoids a fixed `sleep` racing the spawned thread's own scheduling.
        let deadline = Instant::now() + Duration::from_secs(1);
        while app.needs_poll() && Instant::now() < deadline {
            app = app.update(Event::Tick);
        }
        assert!(
            !app.needs_poll(),
            "expected the Tick-drain to clear the pending probe once it resolved"
        );
    }

    #[test]
    fn input_is_never_blocked_by_a_pending_what_probe() {
        let mut wizard = non_sensitive_wizard();
        wizard.what_mut().set_pending_probe(
            PendingProbe::spawn(|| WhatProbeResult::TotalSize(0)),
            ProbeTarget::Category("KDE Plasma"),
        );
        let app = app_with_wizard(wizard);
        assert!(app.needs_poll());

        // Ctrl-D deselects the whole focused Category scope — a real Selection mutation, proving
        // the WHAT reducer still runs a keystroke to completion while a probe is in flight
        // (ADR-0009 D4: input is never blocked by a pending probe).
        let app = app.update(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        let View::Wizard(wizard) = &app.view else {
            panic!("expected the wizard view");
        };
        assert_eq!(
            wizard.state().selection().entries().len(),
            0,
            "expected Ctrl-D to deselect the scope even with a probe pending"
        );
    }

    #[test]
    fn placeholder_screen_renders_its_mode_label() {
        let app = App::new(Screen::Where, test_context(), Glyphs::UNICODE);
        let lines = rendered_lines(&app, 40, 12);
        assert!(
            lines.iter().any(|l| l.contains("where")),
            "expected the mode label, got: {lines:?}"
        );
    }

    /// A `KeyEvent` for driving the wizard reducer through `App::update`.
    fn kev(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }

    /// Types `text` into the focused field by folding one plain-char key per character.
    fn type_into(app: App, text: &str) -> App {
        text.chars().fold(app, |a, c| {
            a.update(kev(KeyCode::Char(c), KeyModifiers::NONE))
        })
    }

    /// A non-Sensitive wizard advanced onto the encryption step with the opt-in encrypt path
    /// selected and its passphrase field focused — the offered-mode entry point.
    fn offered_encryption_typing() -> App {
        app_with_wizard(non_sensitive_wizard())
            .update(kev(KeyCode::Right, KeyModifiers::CONTROL)) // WHAT → Encryption
            .update(kev(KeyCode::Right, KeyModifiers::NONE)) // radio → Encrypt with a passphrase
            .update(kev(KeyCode::Tab, KeyModifiers::NONE)) // focus the passphrase field
    }

    #[test]
    fn question_mark_types_into_the_passphrase_field_instead_of_opening_help() {
        // `?` is no longer a global help key (that rode away to `F1`, #77), so it reaches the
        // focused passphrase field as a literal character — the whole point, since an `age`
        // passphrase may contain one and was previously impossible to set or unlock.
        let app = type_into(offered_encryption_typing(), "pa?ss");
        assert!(!app.help_open, "`?` must not open the overlay while typing");
        assert_eq!(current_step(&app), WizardStep::Encryption);
        let revealed = app.update(kev(KeyCode::F(2), KeyModifiers::NONE));
        let lines = rendered_lines(&revealed, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("pa?ss")),
            "the `?` must be stored in the passphrase, got: {lines:?}"
        );
    }

    #[test]
    fn question_mark_types_into_the_what_custom_path_field_instead_of_opening_help() {
        // The other lock-out field #77 names — the WHAT Custom-path add field — routes through a
        // different `App::update` branch than the passphrase, so it needs its own reducer guard:
        // a re-added global `?` intercept would fail this test where a widget-handler test can't.
        let app = app_with_wizard(non_sensitive_wizard())
            .update(kev(KeyCode::Down, KeyModifiers::NONE)) // onto the Custom paths row
            .update(kev(KeyCode::Right, KeyModifiers::NONE)); // open the add field
        let app = type_into(app, "a?b");
        assert!(
            !app.help_open,
            "`?` must not open the overlay while typing a path"
        );
        assert_eq!(current_step(&app), WizardStep::What);
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("a?b")),
            "the `?` must be stored in the Custom-path buffer, got: {lines:?}"
        );
    }

    #[test]
    fn encryption_offered_screen_masks_the_passphrase_then_reveals_it_on_f2() {
        let app = type_into(offered_encryption_typing(), "hunter2");
        let masked = rendered_lines(&app, 80, 24);
        assert!(
            masked.iter().any(|l| l.contains("•••••••")),
            "expected a masked passphrase, got: {masked:?}"
        );
        assert!(
            !masked.iter().any(|l| l.contains("hunter2")),
            "the plaintext passphrase must not appear while masked, got: {masked:?}"
        );

        // F2 reveals the passphrase for the frame; the same masked render returns once hidden.
        let revealed = app.update(kev(KeyCode::F(2), KeyModifiers::NONE));
        let revealed_lines = rendered_lines(&revealed, 80, 24);
        assert!(
            revealed_lines.iter().any(|l| l.contains("hunter2")),
            "expected the revealed passphrase, got: {revealed_lines:?}"
        );

        // Toggling reveal off again masks identically — the SecretString was never consumed or
        // cleared by a reveal, so no plaintext copy is retained across frames.
        let hidden_again = revealed.update(kev(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(rendered_lines(&hidden_again, 80, 24), masked);
    }

    #[test]
    fn encryption_screen_shows_the_live_match_indicator() {
        // Matching passphrase + confirm → the match indicator.
        let app = type_into(offered_encryption_typing(), "hunter2");
        let app = app.update(kev(KeyCode::Tab, KeyModifiers::NONE)); // focus Confirm
        let app = type_into(app, "hunter2");
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("passphrases match")),
            "expected the match indicator, got: {lines:?}"
        );

        // A differing confirm → the mismatch indicator.
        let mismatch = type_into(offered_encryption_typing(), "hunter2");
        let mismatch = mismatch.update(kev(KeyCode::Tab, KeyModifiers::NONE));
        let mismatch = type_into(mismatch, "hunterX");
        let lines = rendered_lines(&mismatch, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("do not match")),
            "expected the mismatch indicator, got: {lines:?}"
        );
    }

    #[test]
    fn encryption_screen_renders_the_invalidation_banner() {
        // A completed plaintext decision undone by newly-added Sensitive content.
        let mut state = WizardState::new(
            SelectionModel::new(vec![entry("Shell", false, ".zshrc")]),
            Some(PathBuf::from("/home/tester")),
        );
        state.selection_mut().add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );
        // Jump to the collapsed-to encryption step and render its body.
        let app =
            app_with_wizard(Wizard::new(state)).update(kev(KeyCode::Char('2'), KeyModifiers::NONE));
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Sensitive content added")),
            "expected the invalidation banner, got: {lines:?}"
        );
    }

    #[test]
    fn encryption_screen_required_mode_names_sensitive_categories() {
        let app =
            app_with_wizard(sensitive_wizard()).update(kev(KeyCode::Right, KeyModifiers::CONTROL));
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Sensitive Categories")),
            "expected the required-mode Sensitive Categories line, got: {lines:?}"
        );
        // The prose may wrap across rows, so assert against the joined (unpadded) body text.
        let prose = lines
            .iter()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            prose.contains("whole Archive will be encrypted"),
            "expected the required-mode prose, got: {lines:?}"
        );
    }

    #[test]
    fn plaintext_modal_renders_over_the_encryption_body() {
        // Sensitive → required; Tab to the disable button and open the modal.
        let app = app_with_wizard(sensitive_wizard())
            .update(kev(KeyCode::Right, KeyModifiers::CONTROL)) // → Encryption
            .update(kev(KeyCode::Tab, KeyModifiers::NONE)) // Passphrase → Confirm
            .update(kev(KeyCode::Tab, KeyModifiers::NONE)) // Confirm → Disable
            .update(kev(KeyCode::Enter, KeyModifiers::NONE)); // open modal
        let lines = rendered_lines(&app, 80, 24);
        let joined = lines.join("\n");
        assert!(
            joined.contains("Write credentials in plaintext?"),
            "expected the modal title, got: {lines:?}"
        );
        assert!(
            joined.contains("PLAINTEXT"),
            "expected the confirmation prompt, got: {lines:?}"
        );
        assert!(
            joined.contains("[ Cancel ]") && joined.contains("[ Confirm ]"),
            "expected both modal buttons, got: {lines:?}"
        );
    }

    #[test]
    fn encryption_forward_gates_on_a_matched_passphrase() {
        // Opt-in encrypt, type a passphrase without confirming → forward refused, do-not-match.
        let app = type_into(offered_encryption_typing(), "hunter2")
            .update(kev(KeyCode::Right, KeyModifiers::CONTROL));
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Passphrases do not match")),
            "expected the mismatch gate banner, got: {lines:?}"
        );
    }

    // --- Build 11 keymap wiring (#73) ---------------------------------------------------------

    /// The current wizard step, for asserting a key did (or didn't) move the frontier.
    fn current_step(app: &App) -> WizardStep {
        match &app.view {
            View::Wizard(wizard) => wizard.current(),
            other => panic!("expected a wizard view, got: {other:?}"),
        }
    }

    #[test]
    fn reserved_control_chords_are_never_bound() {
        // Ctrl-Z/S/Q/\ are terminal-reserved (#51): the reducer swallows them everywhere, so
        // none quits and none moves the wizard off its opening step.
        for c in ['z', 's', 'q', '\\'] {
            let app = app_with_wizard(non_sensitive_wizard())
                .update(key(KeyCode::Char(c), KeyModifiers::CONTROL));
            assert!(!app.should_quit(), "Ctrl-{c} must not quit");
            assert_eq!(
                current_step(&app),
                WizardStep::What,
                "Ctrl-{c} must be inert"
            );
        }
    }

    #[test]
    fn help_overlay_opens_on_f1_and_closes_on_esc_or_f1() {
        let app = app_with_wizard(non_sensitive_wizard());
        assert!(!app.help_open);
        // `F1` opens the exhaustive reference; the panel and its reserved-chords group render.
        let app = app.update(key(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.help_open);
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Keys")),
            "expected the overlay title, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("Reserved")),
            "the overlay must list the reserved chords, got: {lines:?}"
        );
        // A second `F1` closes it.
        let app = app.update(key(KeyCode::F(1), KeyModifiers::NONE));
        assert!(!app.help_open);
        // `Esc` also closes it, and (per the keymap) still never quits.
        let app = app
            .update(key(KeyCode::F(1), KeyModifiers::NONE))
            .update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.help_open);
        assert!(!app.should_quit());
    }

    #[test]
    fn help_overlay_opens_at_any_size_including_below_refuse() {
        // The overlay is reachable at every terminal size — even below the reflow ladder's
        // Refuse cutoff, where the base render is only the too-small notice (#51).
        for (w, h) in [(80u16, 24u16), (40, 10), (30, 6)] {
            let app = app_with_wizard(non_sensitive_wizard())
                .update(key(KeyCode::F(1), KeyModifiers::NONE));
            let lines = rendered_lines(&app, w, h);
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("Keys") || l.contains("Move")),
                "help overlay must render at {w}x{h}, got: {lines:?}"
            );
        }
    }

    #[test]
    fn f3_toggles_hidden_file_visibility() {
        // `F3` flips the WHERE autocomplete's show-hidden toggle, and stays live regardless of
        // which widget owns the plain keys (#51).
        let app = app_with_wizard(non_sensitive_wizard());
        assert!(!app.where_step.show_hidden());
        let app = app.update(key(KeyCode::F(3), KeyModifiers::NONE));
        assert!(app.where_step.show_hidden());
        let app = app.update(key(KeyCode::F(3), KeyModifiers::NONE));
        assert!(!app.where_step.show_hidden());
    }

    #[test]
    fn digits_reach_a_focused_field_and_do_not_jump_steps() {
        // Forced consequence #1 (#51): a typed digit is inserted into the focused passphrase
        // field rather than jumping steps, so the wizard stays on the encryption step.
        let app = type_into(offered_encryption_typing(), "12");
        assert_eq!(current_step(&app), WizardStep::Encryption);
        let lines = rendered_lines(&app, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("••")),
            "the digits must mask into the field, got: {lines:?}"
        );
        // Contrast: with no field focused (REVIEW), the same digit is the fast step jump.
        let jumped =
            app_with_wizard(wizard_on_review()).update(key(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(current_step(&jumped), WizardStep::What);
    }

    #[test]
    fn a_digit_reaches_the_open_what_custom_path_field_instead_of_jumping() {
        // Open the WHAT Custom-path add field (Down onto the Custom paths row, then →).
        let app = app_with_wizard(non_sensitive_wizard())
            .update(kev(KeyCode::Down, KeyModifiers::NONE))
            .update(kev(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(current_step(&app), WizardStep::What);
        // With the field focused, a typed digit edits the buffer rather than jumping steps (#51).
        let app = app.update(kev(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(current_step(&app), WizardStep::What);
        // But the step-motion chords still move between steps mid-entry.
        let app = app.update(kev(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(current_step(&app), WizardStep::Encryption);
    }

    #[test]
    fn no_emoji_fallback_swaps_the_bullet_mask() {
        // The passphrase mask's `•` is the bullet glyph: Unicode draws `•`, the ASCII set `*`.
        let mut app = type_into(offered_encryption_typing(), "hunter2");
        let unicode = rendered_lines(&app, 80, 24);
        assert!(
            unicode.iter().any(|l| l.contains("•••••••")),
            "expected the Unicode bullet mask, got: {unicode:?}"
        );
        app.glyphs = Glyphs::ASCII;
        let ascii = rendered_lines(&app, 80, 24);
        assert!(
            ascii.iter().any(|l| l.contains("*******")),
            "expected the ASCII bullet mask, got: {ascii:?}"
        );
        assert!(
            !ascii.iter().any(|l| l.contains('•')),
            "the ASCII fallback must show no `•`, got: {ascii:?}"
        );
    }

    #[test]
    fn ctrl_left_moves_a_step_back_even_with_a_field_focused() {
        // Forced consequence #2 (#51): `Ctrl-←` overrides word-left inside a text field — on the
        // encryption step with the passphrase field focused it steps back to WHAT.
        let app = type_into(offered_encryption_typing(), "hunter2")
            .update(kev(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(current_step(&app), WizardStep::What);
    }

    #[test]
    fn no_emoji_fallback_swaps_glyphs_across_the_render() {
        // The stepper's semantic glyphs come from the active set (#51/#61): the Unicode set
        // draws `●`/`·`/`🔒`; the ASCII fallback maps them to `>`/`.`/`[!]` and shows no emoji.
        let mut app = app_with_wizard(sensitive_wizard());
        let unicode = rendered_lines(&app, 80, 24);
        assert!(
            unicode[0].contains('●') && unicode[0].contains('🔒'),
            "expected Unicode stepper glyphs, got: {:?}",
            unicode[0]
        );
        app.glyphs = Glyphs::ASCII;
        let ascii = rendered_lines(&app, 80, 24);
        assert!(
            !ascii[0].contains('●') && !ascii[0].contains('🔒'),
            "the ASCII fallback must drop the emoji glyphs, got: {:?}",
            ascii[0]
        );
        assert!(
            ascii[0].contains('>') && ascii[0].contains("[!]"),
            "expected the ASCII fallback glyphs, got: {:?}",
            ascii[0]
        );
    }
}
