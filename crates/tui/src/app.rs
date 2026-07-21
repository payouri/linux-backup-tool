//! The single `App` reducer and its pure render, per ADR-0005.
//!
//! Spec #62 fixes the architecture this module seeds: one `App` struct in `crates/tui` holds
//! all state, a [`Screen`] enum is its active route, and behaviour is a pure
//! `update(state, Event) -> state` reducer plus a pure render function. That split is what
//! makes event logic unit-testable with hand-built `KeyEvent`s and rendering assertable via
//! `ratatui::backend::TestBackend` — neither needs a real terminal.
//!
//! This is the Foundation build (#63): the reducer only handles quit and resize, and render
//! draws a placeholder frame. The wizard/restore/RUN screens, `WizardState`, and the owned
//! `core::SelectionModel` land in later builds (#62 sequencing steps 3–9); the shapes here
//! are deliberately minimal so those builds extend rather than rework them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::RunContext;
use ratatui::layout::Alignment;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// The active route. `main` seeds one from the parsed CLI command; later builds add the
/// per-step wizard routes and the RUN/result screens around these entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The backup wizard's entry point (WHAT → encryption → WHERE → REVIEW), seeded by
    /// `linux-backup backup`.
    Backup,
    /// The restore flow, seeded by `linux-backup restore <dir>`.
    Restore,
    /// The standalone WHAT screen, seeded by `linux-backup select`.
    Select,
    /// The standalone WHERE screen, seeded by `linux-backup where`.
    Where,
}

impl Screen {
    /// A short, stable label for the placeholder frame and the shared runtime's mode line.
    fn label(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Restore => "restore",
            Self::Select => "select",
            Self::Where => "where",
        }
    }
}

/// Everything the reducer consumes. The shared runtime (`runtime`) translates crossterm's
/// input into these — key presses (already filtered to `KeyEventKind::Press`) and terminal
/// resizes — so `update` never touches the terminal or crossterm's read loop directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
}

/// The whole application state. Foundation holds only the active route, the injected
/// [`RunContext`] (ADR-0004: run facts are injected, never read ambiently by anything under
/// `core`), and the quit flag the runtime watches. The owned `SelectionModel`, `WizardState`,
/// cursor/scroll, and input buffers are added by later builds around this shape.
#[derive(Debug, Clone)]
pub struct App {
    screen: Screen,
    // Held for the wizard/restore screens that read it in later #62 builds; injected now (per
    // ADR-0004) so the runtime seam and `RunContext` ownership are settled by Foundation.
    #[allow(dead_code)]
    run_context: RunContext,
    should_quit: bool,
}

impl App {
    /// Builds the initial state for `screen`, carrying the machine's `run_context`.
    pub fn new(screen: Screen, run_context: RunContext) -> Self {
        Self {
            screen,
            run_context,
            should_quit: false,
        }
    }

    /// Whether the runtime should tear down and exit after the last `update`.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// The pure reducer: fold one [`Event`] into the next state. Taking and returning `Self`
    /// by value keeps it a total function of its inputs — the property the reducer unit tests
    /// rely on — with no `&mut` aliasing or hidden I/O.
    ///
    /// Only `Ctrl-C` (the sole quit, per the #51 keymap) does anything in Foundation; `Esc`
    /// is deliberately inert here (it means "back/close", never quit, and there is no screen
    /// stack yet to pop). Resizes are folded so the runtime repaints, but there is no
    /// size-dependent state to update until the reflow ladder lands (#50).
    #[must_use]
    pub fn update(mut self, event: Event) -> Self {
        if let Event::Key(key) = event
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.should_quit = true;
        }
        self
    }

    /// The pure render: draw the current state into `frame`. Foundation draws a placeholder
    /// so the runtime and the `TestBackend` harness have something to exercise; each real
    /// screen replaces this body in its own build.
    pub fn render(&self, frame: &mut Frame) {
        let body = format!(
            "{} screen — not yet built\npress Ctrl-C to quit",
            self.screen.label()
        );
        let placeholder = Paragraph::new(body)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("linux-backup"));
        frame.render_widget(placeholder, frame.area());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::time::SystemTime;

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
        let app = App::new(Screen::Backup, test_context());
        assert!(!app.should_quit());

        let app = app.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(app.should_quit());
    }

    #[test]
    fn plain_c_does_not_quit() {
        let app = App::new(Screen::Backup, test_context());

        let app = app.update(key(KeyCode::Char('c'), KeyModifiers::NONE));

        assert!(!app.should_quit());
    }

    #[test]
    fn esc_never_quits() {
        let app = App::new(Screen::Restore, test_context());

        let app = app.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert!(!app.should_quit());
    }

    #[test]
    fn resize_is_folded_without_quitting() {
        let app = App::new(Screen::Backup, test_context());

        let app = app.update(Event::Resize(80, 24));

        assert!(!app.should_quit());
    }

    #[test]
    fn renders_a_titled_placeholder_for_the_active_screen() {
        let app = App::new(Screen::Where, test_context());

        let lines = rendered_lines(&app, 40, 6);

        assert!(
            lines.iter().any(|line| line.contains("linux-backup")),
            "expected the frame title, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("where screen")),
            "expected the active screen label, got: {lines:?}"
        );
    }
}
