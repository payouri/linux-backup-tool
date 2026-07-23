//! PROTOTYPE — throwaway, not production code. Answers issue #94: where does the WHAT-step
//! loader spinner sit, what does it say, and how does it dismiss, against the mechanism ADR-0009
//! already settled (spinner reveals at 375ms elapsed, input never blocked, no cancel).
//!
//! Run: `cargo run -p tui --example loader_prototype`
//!
//! A mock of the real two-pane WHAT layout (Categories left, Explorer right — see
//! `src/what_screen.rs`'s `render_categories`/`render_explorer`) with one Explorer row wired to a
//! fake `PendingProbe`. Self-contained: `tui` has no `lib.rs`, so this can't import
//! `what_screen`'s real glyphs/layout — everything needed is copied inline for visual fidelity.
//!
//! Keys:
//!   <-  ->   cycle placement variant (A: inline in the Explorer row / B: status line / C: inline
//!            in the Categories pane)
//!   w        cycle spinner copy (bare glyph / glyph + word)
//!   p        start or settle the probe (mirrors a real probe's spawn + resolve)
//!   q, Esc   quit
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠇'];
const DEBOUNCE: Duration = Duration::from_millis(375);
const POLL_INTERVAL: Duration = Duration::from_millis(80);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    InlineExplorer,
    StatusLine,
    InlineCategories,
}

impl Variant {
    fn next(self) -> Self {
        match self {
            Self::InlineExplorer => Self::StatusLine,
            Self::StatusLine => Self::InlineCategories,
            Self::InlineCategories => Self::InlineExplorer,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::InlineExplorer => Self::InlineCategories,
            Self::StatusLine => Self::InlineExplorer,
            Self::InlineCategories => Self::StatusLine,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::InlineExplorer => "A: inline in the Explorer row's metric column",
            Self::StatusLine => "B: dedicated status line under both panes",
            Self::InlineCategories => {
                "C: inline in the Categories pane (hypothetical — #96 hasn't decided whether a \
                 Category shows a size at all)"
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStyle {
    Bare,
    Worded,
}

impl CopyStyle {
    fn toggled(self) -> Self {
        match self {
            Self::Bare => Self::Worded,
            Self::Worded => Self::Bare,
        }
    }

    fn text(self, spin: char) -> String {
        match self {
            Self::Bare => spin.to_string(),
            Self::Worded => format!("{spin} sizing…"),
        }
    }
}

struct Proto {
    variant: Variant,
    copy: CopyStyle,
    pending_since: Option<Instant>,
    spinner_tick: usize,
    settled_size: &'static str,
}

impl Proto {
    fn spinner_visible(&self) -> Option<char> {
        let since = self.pending_since?;
        (since.elapsed() >= DEBOUNCE).then(|| SPINNER[self.spinner_tick % SPINNER.len()])
    }
}

fn main() -> anyhow::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    crossterm::terminal::enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut proto = Proto {
        variant: Variant::InlineExplorer,
        copy: CopyStyle::Bare,
        pending_since: None,
        spinner_tick: 0,
        settled_size: "4.8 MB",
    };

    let result = run(&mut terminal, &mut proto);

    crossterm::terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    proto: &mut Proto,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, proto))?;

        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Left => proto.variant = proto.variant.prev(),
                    KeyCode::Right => proto.variant = proto.variant.next(),
                    KeyCode::Char('w') => proto.copy = proto.copy.toggled(),
                    KeyCode::Char('p') => {
                        proto.pending_since = match proto.pending_since {
                            None => Some(Instant::now()),
                            Some(_) => None,
                        }
                    }
                    _ => {}
                }
            }
        } else {
            // A poll timeout: mirrors the real `Event::Tick` drain — bump the spinner and
            // reprint so the debounce threshold and the animation are both visible live.
            proto.spinner_tick = proto.spinner_tick.wrapping_add(1);
        }
    }
}

fn render(frame: &mut Frame, proto: &Proto) {
    let area = frame.area();
    let rows = Layout::new(
        Direction::Vertical,
        [
            Constraint::Min(0),
            Constraint::Length(u16::from(proto.variant == Variant::StatusLine)),
            Constraint::Length(3),
        ],
    )
    .split(area);

    let columns = Layout::new(
        Direction::Horizontal,
        [Constraint::Length(28), Constraint::Min(0)],
    )
    .split(rows[0]);

    render_categories(frame, columns[0], proto);
    render_explorer(frame, columns[1], proto);
    render_status_line(frame, rows[1], proto);
    render_caption(frame, rows[2], proto);
}

fn render_categories(frame: &mut Frame, area: Rect, proto: &Proto) {
    let block = Block::default().borders(Borders::ALL).title("Categories");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut kde_line = "▸[x] KDE Plasma  128".to_string();
    if let (Variant::InlineCategories, Some(spin)) = (proto.variant, proto.spinner_visible()) {
        kde_line.push(' ');
        kde_line.push_str(&proto.copy.text(spin));
    }

    let lines = vec![
        Line::from(kde_line),
        Line::from(" [ ] Shell        12"),
        Line::from(" [~] Program configs  340"),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_explorer(frame: &mut Frame, area: Rect, proto: &Proto) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::new(
        Direction::Vertical,
        [Constraint::Length(1), Constraint::Min(0)],
    )
    .split(inner);

    frame.render_widget(
        Paragraph::new("KDE Plasma / .local/share/plasma")
            .style(Style::default().add_modifier(Modifier::DIM)),
        split[0],
    );

    let bar_metric = match proto.variant {
        Variant::InlineExplorer => match proto.spinner_visible() {
            Some(spin) => proto.copy.text(spin),
            None if proto.pending_since.is_some() => String::new(), // < 375ms: no flicker
            None => proto.settled_size.to_string(),
        },
        _ => proto.settled_size.to_string(),
    };

    let lines = vec![
        Line::from(format!("▸[x] bar/            {bar_metric}")),
        Line::from("  [ ] foo.conf          2.1 KB"),
        Line::from("  [x] baz.log           512 B"),
    ];
    frame.render_widget(Paragraph::new(lines), split[1]);
}

fn render_status_line(frame: &mut Frame, area: Rect, proto: &Proto) {
    if proto.variant != Variant::StatusLine || area.height == 0 {
        return;
    }
    let text = match proto.spinner_visible() {
        Some(spin) => proto.copy.text(spin),
        None => String::new(),
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn render_caption(frame: &mut Frame, area: Rect, proto: &Proto) {
    let state = match proto.pending_since {
        None => "settled".to_string(),
        Some(since) if since.elapsed() >= DEBOUNCE => "pending, spinner visible".to_string(),
        Some(since) => format!(
            "pending, debounced ({}ms elapsed, reveals at 375ms)",
            since.elapsed().as_millis()
        ),
    };
    let lines = vec![
        Line::from(vec![
            Span::raw("Variant: "),
            Span::raw(proto.variant.label()),
        ]),
        Line::from(format!(
            "Probe: {state}  |  <- -> variant   w copy   p start/settle probe   q quit"
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}
