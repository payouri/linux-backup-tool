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
//! redraw-on-event loop's step-motion/esc/quit wiring. The per-step screen *bodies* (WHAT,
//! encryption, WHERE, REVIEW) and the restore/RUN screens land in later builds (#68–#72), so
//! the bodies here are placeholders.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::model::SelectionModel;
use linux_backup_core::{scan, RunContext};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::reflow::ReflowTier;
use crate::wizard::{StepStatus, Wizard, WizardState, WizardStep};

/// The entry route `main` seeds from the parsed CLI command. `backup` opens the wizard; the
/// others open their single screen (their bodies land in later builds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The backup wizard (WHAT → encryption → WHERE → REVIEW), seeded by `linux-backup backup`.
    Backup,
    /// The restore flow, seeded by `linux-backup restore <dir>`.
    Restore,
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
            Self::Restore => "restore",
            Self::Select => "select",
            Self::Where => "where",
        }
    }
}

/// The live content behind the frame: either the backup wizard or a not-yet-built screen's
/// placeholder. Later builds replace the placeholders with the restore/RUN/dev screens.
#[derive(Debug, Clone)]
enum View {
    Wizard(Wizard),
    Placeholder(Screen),
}

/// Everything the reducer consumes. The shared runtime (`runtime`) translates crossterm's
/// input into these — key presses (already filtered to `KeyEventKind::Press`) and terminal
/// resizes — so `update` never touches the terminal or crossterm's read loop directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
}

/// The whole application state: the active [`View`], the injected [`RunContext`] (ADR-0004:
/// run facts are injected, never read ambiently by anything under `core`), and the quit flag
/// the runtime watches.
#[derive(Debug, Clone)]
pub struct App {
    view: View,
    // Held for the screens that read it in later builds; injected now (ADR-0004).
    #[allow(dead_code)]
    run_context: RunContext,
    should_quit: bool,
}

impl App {
    /// Builds the initial state for `screen`, carrying the machine's `run_context`. The
    /// `backup` route seeds the wizard from a Smart-default Selection (a `core` scan over the
    /// injected context) and pre-fills WHERE with the context's Output location, so the shell
    /// opens on a populated wizard rather than an empty one.
    #[must_use]
    pub fn new(screen: Screen, run_context: RunContext) -> Self {
        let view = match screen {
            Screen::Backup => View::Wizard(seed_wizard(&run_context)),
            other => View::Placeholder(other),
        };
        Self {
            view,
            run_context,
            should_quit: false,
        }
    }

    /// Whether the runtime should tear down and exit after the last `update`.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// The pure reducer: fold one [`Event`] into the next state. Taking and returning `Self`
    /// by value keeps it a total function of its inputs — the property the reducer unit tests
    /// rely on — with no `&mut` aliasing or hidden I/O.
    ///
    /// `Ctrl-C` is the sole quit (the #51 keymap) and is handled here, before any view sees
    /// the key. Every other key routes to the active view; `Esc` there means back/close and
    /// never quits. Resizes are folded so the loop repaints; the reflow tier is recomputed
    /// from the frame at render time, so there is no size-dependent state to store.
    #[must_use]
    pub fn update(mut self, event: Event) -> Self {
        let Event::Key(key) = event else {
            return self;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return self;
        }
        self.view = match self.view {
            View::Wizard(wizard) => View::Wizard(wizard.handle_key(key)),
            // Placeholder screens have no interactions yet; `Esc` is inert (never quits).
            View::Placeholder(screen) => View::Placeholder(screen),
        };
        self
    }

    /// The pure render: draw the current state into `frame`. Below the reflow ladder's Refuse
    /// cutoff every view collapses to a single full-screen "terminal too small" notice; above
    /// it, the wizard draws its fixed frame and the placeholders a titled mode block.
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let tier = ReflowTier::for_height(area.height);
        if !tier.renders_frame() {
            render_too_small(frame, area);
            return;
        }
        match &self.view {
            View::Wizard(wizard) => render_wizard(frame, area, wizard, tier),
            View::Placeholder(screen) => render_placeholder(frame, area, *screen),
        }
    }
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
fn render_wizard(frame: &mut Frame, area: Rect, wizard: &Wizard, tier: ReflowTier) {
    let banner_rows = u16::from(wizard.gate_banner().is_some());
    let chunks = Layout::vertical([
        Constraint::Length(tier.header_rows()),
        Constraint::Length(banner_rows),
        Constraint::Min(crate::reflow::MIN_BODY_ROWS),
        Constraint::Length(tier.footer_rows()),
    ])
    .split(area);

    render_header(frame, chunks[0], wizard, tier);
    if banner_rows > 0 {
        render_gate_banner(frame, chunks[1], wizard);
    }
    render_body(frame, chunks[2], wizard);
    render_footer(frame, chunks[3], wizard, tier);
}

/// The fixed header: the stepper row, plus the context/status line beneath it at Comfortable.
fn render_header(frame: &mut Frame, area: Rect, wizard: &Wizard, tier: ReflowTier) {
    if tier.shows_context_line() && area.height >= 2 {
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
        frame.render_widget(Paragraph::new(stepper_line(wizard, tier)), rows[0]);
        frame.render_widget(
            Paragraph::new(context_line(wizard))
                .style(Style::default().add_modifier(Modifier::DIM)),
            rows[1],
        );
    } else {
        frame.render_widget(Paragraph::new(stepper_line(wizard, tier)), area);
    }
}

/// The horizontal stepper (#50). Full form spells every step (`glyph Title → …`); the compact
/// form (Tight and below) shows the glyphs plus only the current step's title. The current
/// step's title is marked by color/contrast (not the glyph alone), a Sensitive Selection adds
/// a lock to the Encryption label, and an invalidated encryption step is yellow.
fn stepper_line(wizard: &Wizard, tier: ReflowTier) -> Line<'static> {
    let sensitive = wizard.state().is_sensitive();
    if tier.compact_stepper() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for step in WizardStep::ALL {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(wizard.step_status(step).glyph().to_string()));
        }
        spans.push(Span::raw("  ·  "));
        spans.push(step_title_span(wizard, wizard.current(), sensitive));
        return Line::from(spans);
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, step) in WizardStep::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  →  "));
        }
        let status = wizard.step_status(step);
        spans.push(Span::raw(format!("{} ", status.glyph())));
        spans.push(step_title_span(wizard, step, sensitive));
    }
    Line::from(spans)
}

/// One step's title span, styled by its stepper status (#50). The lock suffix rides the
/// Encryption title when the Selection is Sensitive.
fn step_title_span(wizard: &Wizard, step: WizardStep, sensitive: bool) -> Span<'static> {
    let mut title = step.title().to_string();
    if step == WizardStep::Encryption && sensitive {
        title.push_str(" 🔒");
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

/// The body region — the only part that swaps between steps. Real bodies land in builds
/// #68–#72; the shell shows which step is current.
fn render_body(frame: &mut Frame, area: Rect, wizard: &Wizard) {
    let body = format!(
        "{} — screen body lands in a later build",
        wizard.current().title()
    );
    frame.render_widget(Paragraph::new(body).alignment(Alignment::Center), area);
}

/// The fixed footer hints, degrading down the reflow ladder but always keeping `? keys` (#50).
fn render_footer(frame: &mut Frame, area: Rect, wizard: &Wizard, tier: ReflowTier) {
    let forward_hint = if wizard.state().step_complete(wizard.current()) {
        "Ctrl-→ next"
    } else {
        // The forward chord dims when the current step is incomplete (#61).
        "Ctrl-→ next (locked)"
    };
    let hints = match tier {
        ReflowTier::Comfortable | ReflowTier::Default => {
            format!("{forward_hint} · Ctrl-← back · 1-4 jump · Esc back · ? keys")
        }
        ReflowTier::Tight => format!("{forward_hint} · Ctrl-← back · ? keys"),
        ReflowTier::Minimum | ReflowTier::Refuse => format!("{forward_hint} · ? keys"),
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
    use std::path::PathBuf;
    use std::time::SystemTime;

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
            run_context: test_context(),
            should_quit: false,
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
        let app = App::new(Screen::Restore, test_context());
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
                lines.iter().any(|l| l.contains("? keys")),
                "footer lost `? keys` at height {height}, got: {lines:?}"
            );
        }
    }

    #[test]
    fn placeholder_screen_renders_its_mode_label() {
        let app = App::new(Screen::Where, test_context());
        let lines = rendered_lines(&app, 40, 12);
        assert!(
            lines.iter().any(|l| l.contains("where")),
            "expected the mode label, got: {lines:?}"
        );
    }
}
