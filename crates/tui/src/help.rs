//! The `F1` help overlay: the single exhaustive keybinding reference (#51 / #61 / #77 / #84).
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

use crate::keymap::{self, Binding, Group};

/// The overlay's `(heading, keys)` rows, derived from the keymap catalog (#74) so the reference
/// is never a hand-maintained second copy of the footer's labels. Each group's bindings are the
/// catalog entries flagged with that group, joined with the catalog separator; a blank row spaces
/// the reserved group off, which prints [`keymap::RESERVED`] behind a prose lead-in.
fn rows() -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = keymap::OVERLAY_GROUPS
        .iter()
        .map(|&group| (group.heading(), group_keys(group)))
        .collect();
    rows.push(("", String::new()));
    let reserved = keymap::RESERVED
        .iter()
        .map(|chord| chord.spelling)
        .collect::<Vec<_>>()
        .join(keymap::SEP);
    rows.push((
        Group::Reserved.heading(),
        format!("never bound by the tool: {reserved}"),
    ));
    rows
}

/// The joined key labels for one overlay group, in catalog order.
fn group_keys(group: Group) -> String {
    keymap::CATALOG
        .iter()
        .filter(|binding| binding.group == Some(group))
        .map(Binding::label)
        .collect::<Vec<_>>()
        .join(keymap::SEP)
}

/// How many bindings the overlay lists (used by the app to clamp the scroll offset).
#[must_use]
pub fn line_count() -> u16 {
    // The group rows plus the trailing "close" hint line.
    u16::try_from(rows().len())
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

    let mut lines: Vec<Line<'static>> = rows()
        .into_iter()
        .map(|(label, keys)| {
            if label.is_empty() && keys.is_empty() {
                Line::default()
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{label:<11}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(keys),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard (overlay half of #74): every catalog binding flagged in-overlay renders in the
    /// `?` panel, under its group's row. A binding added to the catalog with a group but missed by
    /// the overlay render fails here.
    #[test]
    fn every_overlay_binding_renders_in_the_panel() {
        let rendered = rows()
            .into_iter()
            .map(|(_, keys)| keys)
            .collect::<Vec<_>>()
            .join("\n");
        for binding in keymap::CATALOG {
            if binding.group.is_some() {
                let label = binding.label();
                assert!(
                    rendered.contains(&label),
                    "overlay is missing catalog binding {label:?}"
                );
            }
        }
    }

    /// Reserved single-source (overlay half of #74): every [`keymap::RESERVED`] chord is printed in
    /// the panel, from the same set the swallow guard reads.
    #[test]
    fn reserved_chords_render_in_the_panel() {
        let rendered = rows()
            .into_iter()
            .map(|(heading, keys)| format!("{heading} {keys}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains(Group::Reserved.heading()));
        for chord in keymap::RESERVED {
            assert!(
                rendered.contains(chord.spelling),
                "overlay is missing reserved chord {:?}",
                chord.spelling
            );
        }
    }
}
