//! The `F1` help overlay: the single exhaustive keybinding reference (#51 / #61 / #77).
//!
//! Every binding the tool has — the cross-screen keymap plus the modal and RUN context keys —
//! lives here in one panel, so the footer only ever needs to carry a context subset and always
//! keep `F1 keys` pointing at this. It opens at **every** terminal size (the app renders it even
//! below the reflow ladder's Refuse cutoff, where nothing else draws) and closes on `Esc` or a
//! second `F1`; a short terminal scrolls the panel rather than dropping bindings. It rides `F1`,
//! not the plain `?` the map first used, so a focused text field keeps `?` as a literal character
//! (an `age` passphrase may contain one, #77).
//!
//! Arrows and the `·` separators are left Unicode per the #61 fallback map (only decorative
//! glyphs remap), so the reference reads the same in both glyph sets and needs no glyph set
//! threaded in.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// The grouped binding lines, in the prototype's order (#51 §4) extended with the modal and RUN
/// context groups so the overlay is exhaustive. `(label, keys)` per row; a blank `label` is a
/// spacer.
const GROUPS: &[(&str, &str)] = &[
    ("Move", "↑↓ · PgUp/PgDn · Home/End · type to jump"),
    (
        "Panes",
        "Tab / Shift-Tab focus · →/Enter into dir · ←/Backspace up",
    ),
    (
        "Select",
        "Space toggle (dir = whole subtree) · Ctrl-A all · Ctrl-D none",
    ),
    (
        "Steps",
        "Ctrl-→ next · Ctrl-← back · 1-4 jump · REVIEW: Enter start",
    ),
    ("Reveal", "F2 passphrase · F3 hidden files"),
    ("Global", "F1 keys · Esc back/close · Ctrl-C quit/abort"),
    (
        "Modals",
        "←/→ choose · Enter activate · Esc cancel · type PLAINTEXT",
    ),
    ("RUN", "Ctrl-C cancel · Enter close (when done)"),
    ("", ""),
    (
        "Reserved",
        "never bound by the tool: Ctrl-Z · Ctrl-S · Ctrl-Q · Ctrl-\\",
    ),
];

/// How many bindings the overlay lists (used by the app to clamp the scroll offset).
#[must_use]
pub fn line_count() -> u16 {
    // The groups plus the trailing "close" hint line.
    u16::try_from(GROUPS.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1)
}

/// Draws the overlay over `area`, scrolled by `scroll` rows. Clears its own panel first so it
/// reads as a modal over whatever is behind it. At a size too small for the whole panel the
/// content clips and `scroll` brings the rest into view (the app clamps `scroll` to
/// [`line_count`]).
pub fn render(frame: &mut Frame, area: Rect, scroll: u16) {
    // Wide enough for the longest binding row, but never wider than the terminal.
    let width = area.width.min(80);
    // Room for every group plus the two border rows; clamped to the terminal so a short screen
    // gets a full-height panel it can scroll rather than an over-tall one.
    let height = area.height.min(line_count().saturating_add(2));
    let panel = centered(width, height, area);

    let mut lines: Vec<Line<'static>> = GROUPS
        .iter()
        .map(|(label, keys)| {
            if label.is_empty() && keys.is_empty() {
                Line::default()
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{label:<9}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(*keys),
                ])
            }
        })
        .collect();
    lines.push(Line::from(Span::styled(
        "Esc or F1 to close",
        Style::default().add_modifier(Modifier::DIM),
    )));

    let block = Block::default().borders(Borders::ALL).title("Keys");
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        panel,
    );
}

/// A `width`×`height` rectangle centered in `area`, clamped to fit.
fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let [horizontal] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [vertical] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(horizontal);
    vertical
}
