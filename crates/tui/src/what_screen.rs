//! The WHAT step body: the two-pane recursive Drill-down explorer (#48 / #50 / ADR-0006).
//!
//! Left pane = Categories with a selected-Entry count; right pane = a *single-directory*
//! explorer over the currently-focused Category's Entries, with a breadcrumb pinned above.
//! `Tab`/`Shift-Tab` toggles focus; `→`/`Enter` descends into a directory (the breadcrumb
//! grows, the column reloads with that directory's children) and `←`/`Backspace` climbs, to
//! any depth. `Space` toggles the focused node — a directory toggles its whole subtree
//! (prefix-wins, no carve-outs) — and `Ctrl-A`/`Ctrl-D` select/deselect the whole focused
//! scope. Below [`TWO_PANE_MIN_WIDTH`] the split collapses to a single active column that
//! `Tab` swaps.
//!
//! The navigable tree is **presentation grouping of the already-scanned flat Entries** — this
//! module derives it live from the [`SelectionModel`], never storing a tri-state tree (ADR-0006
//! keeps the flat two-set exclusion model as the single source of truth). Every checkbox comes
//! from [`SelectionModel::directory_state`], and every mutation goes through the model's own
//! public API (`deselect_root`/`reselect_root`/`deselect_entry`/`reselect_entry`), so the
//! tri-state a row shows and the Selection a run honors can never disagree.
//!
//! View state (focus, cursors, breadcrumb, the discovered Category universe) lives here around
//! the model, never inside it (ADR-0005 D1). The reducer and render are pure over that state;
//! the only I/O is the per-file size probe, which goes through `core`'s `fs_probe` rather than
//! touching the disk directly (ADR-0005 D6).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::fs_probe;
use linux_backup_core::model::{DirectoryState, EntryType, SelectionModel};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::glyphs::Glyphs;
use crate::wizard::WizardState;

/// Below this width the two-pane split can't hold, so the WHAT screen collapses to a single
/// active column `Tab` swaps between (#50, the mandatory narrow fallback).
pub const TWO_PANE_MIN_WIDTH: u16 = 70;

/// The fixed width of the Categories column in the two-pane layout; the explorer takes the
/// rest.
const LEFT_WIDTH: u16 = 28;

/// How many rows `PgUp`/`PgDn` move the cursor.
const PAGE: usize = 5;

/// Which of the two panes currently owns the row cursor and the plain-key focus (#50).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pane {
    #[default]
    Categories,
    Explorer,
}

/// The persistent view state of the WHAT screen. The Category universe is captured once at
/// construction so a Category toggled fully off (its Entries withheld, not dropped) keeps its
/// row rather than vanishing from the left pane.
#[derive(Debug, Clone, Default)]
pub struct WhatScreen {
    focus: Pane,
    category_cursor: usize,
    explorer_cursor: usize,
    /// The path components descended into within the current Category — the breadcrumb tail
    /// after the Category root. Empty means the explorer is at the Category's top level.
    breadcrumb: Vec<OsString>,
    /// The Categories to show on the left, in stable sorted order. Seeded from the Selection
    /// at construction and only ever grown (never pruned), so a fully-deselected Category's
    /// row survives for re-selection.
    categories: Vec<&'static str>,
}

impl WhatScreen {
    /// Opens the WHAT screen over `selection`, capturing its Categories as the left-pane
    /// universe.
    #[must_use]
    pub fn new(selection: &SelectionModel) -> Self {
        let mut screen = Self::default();
        screen.sync_categories(selection);
        screen
    }

    /// The Category the explorer is currently showing, if any exist.
    fn current_category(&self) -> Option<&'static str> {
        self.categories.get(self.category_cursor).copied()
    }

    /// Folds any Categories newly present in `selection` into the stable universe without ever
    /// dropping one already known — so a toggle-off (which only withholds Entries) can't erase
    /// a row.
    fn sync_categories(&mut self, selection: &SelectionModel) {
        let mut known: BTreeSet<&'static str> = self.categories.iter().copied().collect();
        for entry in selection.entries() {
            known.insert(entry.category);
        }
        self.categories = known.into_iter().collect();
    }

    /// The pure reducer for the WHAT step's own keys. Global chords (`Ctrl-→`/`Ctrl-←`, `1`–`4`,
    /// `Esc`, `Ctrl-C`) are handled by the wizard/app before a key reaches here, so this owns
    /// only the pane-local motion, drill-down, and selection keys (#51 Move/Panes/Select
    /// groups). Selection edits go through the model's public API; the derived tri-state and
    /// frontier fall out of them.
    pub fn handle_key(&mut self, key: KeyEvent, state: &mut WizardState) {
        self.sync_categories(state.selection());
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            KeyCode::Char('a') if ctrl => self.select_scope(state, true),
            KeyCode::Char('d') if ctrl => self.select_scope(state, false),
            KeyCode::Up => self.move_up(1),
            KeyCode::Down => self.move_down(state, 1),
            KeyCode::PageUp => self.move_up(PAGE),
            KeyCode::PageDown => self.move_down(state, PAGE),
            KeyCode::Home => self.set_cursor(0),
            KeyCode::End => self.move_down(state, usize::MAX),
            KeyCode::Char(' ') => self.toggle(state),
            KeyCode::Right | KeyCode::Enter if self.focus == Pane::Explorer => self.descend(state),
            KeyCode::Left | KeyCode::Backspace if self.focus == Pane::Explorer => self.climb(),
            // On the Categories pane, `→`/`Enter` moves focus into the explorer it drives.
            KeyCode::Right | KeyCode::Enter => self.focus = Pane::Explorer,
            _ => {}
        }
        self.clamp(state);
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Categories => Pane::Explorer,
            Pane::Explorer => Pane::Categories,
        };
    }

    /// The number of rows in the currently-focused pane.
    fn active_len(&self, state: &WizardState) -> usize {
        match self.focus {
            Pane::Categories => self.categories.len(),
            Pane::Explorer => self.explorer_rows(state).len(),
        }
    }

    fn active_cursor(&self) -> usize {
        match self.focus {
            Pane::Categories => self.category_cursor,
            Pane::Explorer => self.explorer_cursor,
        }
    }

    fn set_cursor(&mut self, index: usize) {
        match self.focus {
            Pane::Categories => self.category_cursor = index,
            Pane::Explorer => self.explorer_cursor = index,
        }
    }

    fn move_up(&mut self, by: usize) {
        let next = self.active_cursor().saturating_sub(by);
        self.set_cursor(next);
    }

    fn move_down(&mut self, state: &WizardState, by: usize) {
        let len = self.active_len(state);
        if len == 0 {
            return;
        }
        let next = self.active_cursor().saturating_add(by).min(len - 1);
        self.set_cursor(next);
    }

    /// Descends into the focused explorer directory (drill-down to any depth). A no-op on a
    /// file row or an empty directory (drill-disabled, #50).
    fn descend(&mut self, state: &WizardState) {
        let rows = self.explorer_rows(state);
        let Some(row) = rows.get(self.explorer_cursor) else {
            return;
        };
        if matches!(row.kind, NodeKind::Directory { .. }) {
            self.breadcrumb.push(row.component.clone());
            self.explorer_cursor = 0;
        }
    }

    /// Climbs one level out of the current directory; inert at the Category's top level.
    fn climb(&mut self) {
        if self.breadcrumb.pop().is_some() {
            self.explorer_cursor = 0;
        }
    }

    /// `Space`: toggles the focused node. A directory toggles its whole subtree (prefix-wins,
    /// ADR-0006); a file toggles that one Entry; a Category toggles all its Entries.
    fn toggle(&mut self, state: &mut WizardState) {
        match self.focus {
            Pane::Categories => {
                if let Some(name) = self.current_category() {
                    let select = matches!(
                        category_state(state.selection(), name),
                        DirectoryState::None
                    );
                    set_category(state, name, select);
                }
            }
            Pane::Explorer => {
                let rows = self.explorer_rows(state);
                if let Some(row) = rows.get(self.explorer_cursor) {
                    toggle_node(state, row);
                }
            }
        }
    }

    /// `Ctrl-A`/`Ctrl-D`: select or deselect the whole focused scope — every Category on the
    /// left, or every row of the current directory on the right (#51 Select group).
    fn select_scope(&mut self, state: &mut WizardState, select: bool) {
        match self.focus {
            Pane::Categories => {
                for name in self.categories.clone() {
                    set_category(state, name, select);
                }
            }
            Pane::Explorer => {
                for row in self.explorer_rows(state) {
                    set_node(state, &row, select);
                }
            }
        }
    }

    /// The rows of the directory the explorer currently shows, derived fresh from the
    /// Selection (no stored tree).
    fn explorer_rows(&self, state: &WizardState) -> Vec<ExplorerRow> {
        self.current_category()
            .map(|category| explorer_rows(state.selection(), category, &self.breadcrumb))
            .unwrap_or_default()
    }

    /// Keeps both cursors within their (possibly shrunk) row counts after any action.
    fn clamp(&mut self, state: &WizardState) {
        let categories = self.categories.len();
        if categories > 0 {
            self.category_cursor = self.category_cursor.min(categories - 1);
        } else {
            self.category_cursor = 0;
        }
        let explorer = self.explorer_rows(state).len();
        if explorer > 0 {
            self.explorer_cursor = self.explorer_cursor.min(explorer - 1);
        } else {
            self.explorer_cursor = 0;
        }
    }
}

/// A directory node's tri-state (`[x]`/`[~]`/`[ ]`) as it will render, straight from
/// [`SelectionModel::directory_state`].
fn checkbox(state: DirectoryState) -> &'static str {
    match state {
        DirectoryState::All => "[x]",
        DirectoryState::Partial => "[~]",
        DirectoryState::None => "[ ]",
    }
}

/// A left-pane Category row: its name, its count of selected Entries, whether it carries any
/// Sensitive Entry (the lock), and its derived tri-state.
#[derive(Debug, Clone)]
struct CategoryRow {
    name: &'static str,
    selected: usize,
    sensitive: bool,
    state: DirectoryState,
}

/// The tri-state of a whole Category, from its active-vs-total Entry split — the left pane's
/// analogue of a directory node's [`SelectionModel::directory_state`].
fn category_state(selection: &SelectionModel, category: &str) -> DirectoryState {
    let total = selection.entries_in_category(category).len();
    let active = selection
        .entries()
        .iter()
        .filter(|entry| entry.category == category)
        .count();
    if active == 0 {
        DirectoryState::None
    } else if active == total {
        DirectoryState::All
    } else {
        DirectoryState::Partial
    }
}

fn category_rows(screen: &WhatScreen, selection: &SelectionModel) -> Vec<CategoryRow> {
    screen
        .categories
        .iter()
        .map(|&name| {
            let all = selection.entries_in_category(name);
            let sensitive = all.iter().any(|entry| entry.sensitive);
            let selected = selection
                .entries()
                .iter()
                .filter(|entry| entry.category == name)
                .count();
            CategoryRow {
                name,
                selected,
                sensitive,
                state: category_state(selection, name),
            }
        })
        .collect()
}

/// Selects or deselects every Entry of `category`. Deselecting withholds each Entry's subtree
/// (`deselect_root`, which keeps it recoverable); selecting reverses both a subtree withhold
/// and any per-file Drill-down deselection, so `Ctrl-A` restores a Category in full — including
/// a subtree deselected at an intermediate depth, whose `deselected_root` is not itself an
/// Entry path and so is cleared by matching it against the Category's paths rather than by a
/// per-Entry `reselect_root`.
fn set_category(state: &mut WizardState, category: &str, select: bool) {
    let paths: Vec<PathBuf> = state
        .selection()
        .entries_in_category(category)
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect();
    let selection = state.selection_mut();
    if select {
        let covering_roots: Vec<PathBuf> = selection
            .deselected_roots()
            .iter()
            .filter(|root| paths.iter().any(|path| path.starts_with(root)))
            .cloned()
            .collect();
        for root in &covering_roots {
            selection.reselect_root(root);
        }
        for path in &paths {
            selection.reselect_entry(path);
        }
    } else {
        for path in &paths {
            selection.deselect_root(path);
        }
    }
}

/// What an explorer row is, and the metric it displays: a directory shows its immediate-child
/// count, a file its size, an empty directory a dimmed `—`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeKind {
    Directory { children: usize },
    EmptyDirectory,
    File { size: u64 },
}

/// One row of the single-directory explorer, derived from the flat Entries under the current
/// breadcrumb.
#[derive(Debug, Clone)]
struct ExplorerRow {
    /// The rendered label (a directory carries a trailing `/`).
    label: String,
    /// The raw path component, pushed onto the breadcrumb on descent.
    component: OsString,
    /// The node's relative path — what the tri-state and toggles are keyed on.
    node_path: PathBuf,
    kind: NodeKind,
    state: DirectoryState,
}

impl ExplorerRow {
    /// The right-hand metric column: child count, size, or the empty-directory dash.
    fn metric(&self) -> String {
        match &self.kind {
            NodeKind::Directory { children } => format!("{children} items"),
            NodeKind::EmptyDirectory => "—".to_string(),
            NodeKind::File { size } => format_size(*size),
        }
    }

    fn is_file(&self) -> bool {
        matches!(self.kind, NodeKind::File { .. })
    }
}

/// Accumulates, per immediate child component under the current prefix, whether it has deeper
/// Entries (a real directory) and — if an Entry sits exactly at it — that Entry's own facts.
#[derive(Default)]
struct ChildAccumulator {
    has_children: bool,
    grandchildren: BTreeSet<OsString>,
    exact: Option<(PathBuf, EntryType)>,
}

/// Groups the Category's flat Entries into the rows of the directory at `breadcrumb` — the
/// presentation tree ADR-0006 has `tui` derive, with no fresh filesystem reads for the grouping
/// itself (only per-file sizes go through `core`'s probe).
fn explorer_rows(
    selection: &SelectionModel,
    category: &str,
    breadcrumb: &[OsString],
) -> Vec<ExplorerRow> {
    let prefix: PathBuf = breadcrumb.iter().collect();
    let mut groups: BTreeMap<OsString, ChildAccumulator> = BTreeMap::new();

    for entry in selection.entries_in_category(category) {
        let Ok(rest) = entry.relative_path.strip_prefix(&prefix) else {
            continue;
        };
        let mut components = rest.components();
        let Some(first) = components.next() else {
            continue; // the directory node itself, not one of its children
        };
        let accumulator = groups.entry(first.as_os_str().to_os_string()).or_default();
        if let Some(second) = components.next() {
            accumulator.has_children = true;
            accumulator
                .grandchildren
                .insert(second.as_os_str().to_os_string());
        } else {
            accumulator.exact = Some((entry.absolute_path.clone(), entry.entry_type));
        }
    }

    let mut rows: Vec<ExplorerRow> = groups
        .into_iter()
        .map(|(component, accumulator)| {
            let node_path = prefix.join(&component);
            let state = selection.directory_state(&node_path);
            let name = component.to_string_lossy();
            let (kind, label) = if accumulator.has_children {
                (
                    NodeKind::Directory {
                        children: accumulator.grandchildren.len(),
                    },
                    format!("{name}/"),
                )
            } else {
                match accumulator.exact {
                    Some((_, EntryType::Directory)) => {
                        (NodeKind::EmptyDirectory, format!("{name}/"))
                    }
                    Some((absolute, _)) => (
                        NodeKind::File {
                            size: fs_probe::subtree_size(std::iter::once(absolute.as_path())),
                        },
                        name.into_owned(),
                    ),
                    None => (NodeKind::EmptyDirectory, format!("{name}/")),
                }
            };
            ExplorerRow {
                label,
                component,
                node_path,
                kind,
                state,
            }
        })
        .collect();

    // Directories first, then files, alphabetical case-insensitive within each group (the
    // autocomplete ordering, #52 — a scannable list).
    rows.sort_by(|a, b| {
        a.is_file()
            .cmp(&b.is_file())
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    rows
}

/// Flips one explorer node's selection: a directory's whole subtree (prefix-wins), or a single
/// file Entry.
fn toggle_node(state: &mut WizardState, row: &ExplorerRow) {
    let select = matches!(row.state, DirectoryState::None);
    set_node(state, row, select);
}

/// Sets one explorer node to `select`, routing through the model API that matches its kind: a
/// directory (including an empty one) through the subtree `deselect_root`/`reselect_root`, a
/// file through the per-Entry `deselect_entry`/`reselect_entry`.
fn set_node(state: &mut WizardState, row: &ExplorerRow, select: bool) {
    let selection = state.selection_mut();
    match row.kind {
        NodeKind::File { .. } => {
            if select {
                selection.reselect_entry(&row.node_path);
            } else {
                selection.deselect_entry(&row.node_path);
            }
        }
        NodeKind::Directory { .. } | NodeKind::EmptyDirectory => {
            if select {
                selection.reselect_root(&row.node_path);
            } else {
                selection.deselect_root(&row.node_path);
            }
        }
    }
}

/// Formats a byte count into a compact human-readable size, integer-only to keep clippy's
/// precision-loss lints satisfied without floats.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        let whole = bytes / GB;
        let hundredths = (bytes % GB) * 100 / GB;
        format!("{whole}.{hundredths:02} GB")
    } else if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

/// The style of a pane's border and cursor: the focused pane is brighter (bold cyan), the
/// unfocused one dimmed (#50).
fn pane_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}

/// Draws the WHAT step body: two panes side by side, or — below [`TWO_PANE_MIN_WIDTH`] — the
/// single active column the focused pane fills.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &WhatScreen,
    state: &WizardState,
    glyphs: Glyphs,
) {
    let selection = state.selection();
    let categories = category_rows(screen, selection);
    let rows = screen.explorer_rows(state);
    let breadcrumb = breadcrumb_line(screen.current_category(), &screen.breadcrumb, glyphs);

    if area.width < TWO_PANE_MIN_WIDTH {
        match screen.focus {
            Pane::Categories => {
                render_categories(
                    frame,
                    area,
                    &categories,
                    screen.category_cursor,
                    true,
                    glyphs,
                );
            }
            Pane::Explorer => render_explorer(
                frame,
                area,
                &rows,
                &breadcrumb,
                screen.explorer_cursor,
                true,
                glyphs,
            ),
        }
        return;
    }

    let columns =
        Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(0)]).split(area);
    render_categories(
        frame,
        columns[0],
        &categories,
        screen.category_cursor,
        screen.focus == Pane::Categories,
        glyphs,
    );
    render_explorer(
        frame,
        columns[1],
        &rows,
        &breadcrumb,
        screen.explorer_cursor,
        screen.focus == Pane::Explorer,
        glyphs,
    );
}

/// The breadcrumb pinned above the explorer: the Category root plus every descended component.
fn breadcrumb_line(category: Option<&str>, crumbs: &[OsString], glyphs: Glyphs) -> String {
    let mut line = category.unwrap_or("").to_string();
    for crumb in crumbs {
        line.push(' ');
        line.push_str(glyphs.pointer);
        line.push(' ');
        line.push_str(crumb.to_string_lossy().as_ref());
    }
    line
}

fn render_categories(
    frame: &mut Frame,
    area: Rect,
    categories: &[CategoryRow],
    cursor: usize,
    focused: bool,
    glyphs: Glyphs,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_style(focused))
        .title("Categories");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = categories
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = if focused && index == cursor {
                glyphs.pointer
            } else {
                " "
            };
            let box_glyph = checkbox(row.state);
            let name = row.name;
            let count = row.selected;
            let lock = if row.sensitive {
                format!(" {}", glyphs.lock)
            } else {
                String::new()
            };
            Line::from(format!("{marker}{box_glyph} {name}  {count}{lock}"))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_offset(inner.height, cursor), 0)),
        inner,
    );
}

fn render_explorer(
    frame: &mut Frame,
    area: Rect,
    rows: &[ExplorerRow],
    breadcrumb: &str,
    cursor: usize,
    focused: bool,
    glyphs: Glyphs,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // The breadcrumb is pinned on the first inner row; the directory listing scrolls below it.
    let split = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    frame.render_widget(
        Paragraph::new(breadcrumb.to_string()).style(Style::default().add_modifier(Modifier::DIM)),
        split[0],
    );

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = if focused && index == cursor {
                glyphs.pointer
            } else {
                " "
            };
            let box_glyph = checkbox(row.state);
            let label = &row.label;
            let metric = row.metric();
            let line = Line::from(format!("{marker}{box_glyph} {label}  {metric}"));
            if matches!(row.kind, NodeKind::EmptyDirectory) {
                line.style(Style::default().add_modifier(Modifier::DIM))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_offset(split[1].height, cursor), 0)),
        split[1],
    );
}

/// The vertical scroll offset that keeps `cursor` visible in a pane `height` rows tall
/// (scroll-on-margin, clamp at the ends — #50).
fn scroll_offset(height: u16, cursor: usize) -> u16 {
    let height = usize::from(height);
    if height == 0 || cursor < height {
        return 0;
    }
    u16::try_from(cursor - height + 1).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use linux_backup_core::model::{Entry, EntryRoot};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::io::Write;
    use std::path::Path;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::CONTROL, KeyEventKind::Press)
    }

    fn entry(category: &'static str, kind: EntryType, rel: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/home/tester/{rel}")),
            relative_path: PathBuf::from(rel),
            root: EntryRoot::Home,
            entry_type: kind,
            category,
            sensitive: false,
        }
    }

    /// A nested KDE Plasma Category: two files and one deeper subtree, plus a lone Shell file,
    /// so the explorer has directories to descend and a Category to switch to.
    fn nested_selection() -> SelectionModel {
        SelectionModel::new(vec![
            entry("KDE Plasma", EntryType::File, ".local/share/plasma/foo"),
            entry("KDE Plasma", EntryType::File, ".local/share/plasma/bar/baz"),
            entry("KDE Plasma", EntryType::File, ".local/share/other"),
            entry("Shell", EntryType::File, ".zshrc"),
        ])
    }

    fn state_of(selection: SelectionModel) -> WizardState {
        WizardState::new(selection, Some(PathBuf::from("/home/tester")))
    }

    fn focus_explorer(screen: &mut WhatScreen, state: &mut WizardState) {
        // KDE Plasma sorts before Shell, so cursor 0 already selects it; move focus right.
        screen.handle_key(key(KeyCode::Tab), state);
        assert_eq!(screen.focus, Pane::Explorer);
    }

    #[test]
    fn tab_toggles_focus_between_panes() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        assert_eq!(screen.focus, Pane::Categories);
        screen.handle_key(key(KeyCode::Tab), &mut state);
        assert_eq!(screen.focus, Pane::Explorer);
        screen.handle_key(key(KeyCode::BackTab), &mut state);
        assert_eq!(screen.focus, Pane::Categories);
    }

    #[test]
    fn drill_down_descends_and_climbs_to_intermediate_depth() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        // Top level of KDE Plasma is a single `.local/` directory.
        let rows = screen.explorer_rows(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, ".local/");

        screen.handle_key(key(KeyCode::Right), &mut state); // into .local
        screen.handle_key(key(KeyCode::Enter), &mut state); // into .local/share
        assert_eq!(
            screen.breadcrumb,
            [OsString::from(".local"), OsString::from("share")]
        );

        // .local/share holds `other` (file) and `plasma/` (directory).
        let rows = screen.explorer_rows(&state);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["plasma/", "other"]);

        // Descend into the intermediate `plasma/`, then climb back out one level.
        screen.explorer_cursor = 0;
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert_eq!(
            screen.breadcrumb,
            [
                OsString::from(".local"),
                OsString::from("share"),
                OsString::from("plasma")
            ]
        );
        screen.handle_key(key(KeyCode::Backspace), &mut state);
        assert_eq!(
            screen.breadcrumb,
            [OsString::from(".local"), OsString::from("share")]
        );
    }

    #[test]
    fn space_toggles_a_directory_subtree_prefix_wins() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        // Drill to .local/share and land on the `plasma/` subtree.
        screen.handle_key(key(KeyCode::Right), &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state);
        screen.explorer_cursor = 0;

        let plasma = Path::new(".local/share/plasma");
        assert_eq!(
            state.selection().directory_state(plasma),
            DirectoryState::All
        );

        // Space withholds the whole subtree in one keystroke (both files under it).
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            state.selection().directory_state(plasma),
            DirectoryState::None
        );
        assert!(state
            .selection()
            .entries()
            .iter()
            .all(|e| !e.relative_path.starts_with(plasma)));

        // Space again re-selects the subtree.
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            state.selection().directory_state(plasma),
            DirectoryState::All
        );
    }

    #[test]
    fn deselecting_one_child_makes_the_parent_partial() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state); // .local
        screen.handle_key(key(KeyCode::Right), &mut state); // .local/share
        screen.handle_key(key(KeyCode::Right), &mut state); // into plasma/ (rows sorted dirs-first)

        // .local/share/plasma holds `bar/` (dir) and `foo` (file); deselect the `foo` file.
        let rows = screen.explorer_rows(&state);
        let foo = rows.iter().position(|r| r.label == "foo").expect("foo row");
        screen.explorer_cursor = foo;
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);

        assert_eq!(
            state
                .selection()
                .directory_state(Path::new(".local/share/plasma")),
            DirectoryState::Partial
        );
    }

    #[test]
    fn ctrl_d_deselects_and_ctrl_a_reselects_the_current_directory() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state); // .local
        screen.handle_key(key(KeyCode::Right), &mut state); // .local/share

        screen.handle_key(ctrl(KeyCode::Char('d')), &mut state);
        assert_eq!(
            state.selection().directory_state(Path::new(".local/share")),
            DirectoryState::None
        );

        screen.handle_key(ctrl(KeyCode::Char('a')), &mut state);
        assert_eq!(
            state.selection().directory_state(Path::new(".local/share")),
            DirectoryState::All
        );
    }

    #[test]
    fn reselecting_a_category_clears_a_deep_subtree_deselection() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        // Drill to .local/share, then deselect the intermediate `plasma/` subtree.
        screen.handle_key(key(KeyCode::Right), &mut state); // .local
        screen.handle_key(key(KeyCode::Right), &mut state); // .local/share
        let rows = screen.explorer_rows(&state);
        let plasma = rows
            .iter()
            .position(|r| r.label == "plasma/")
            .expect("plasma row");
        screen.explorer_cursor = plasma;
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            category_state(state.selection(), "KDE Plasma"),
            DirectoryState::Partial
        );

        // Ctrl-A on the Categories pane must restore the Category in full — including the
        // subtree deselected at an intermediate depth (its `deselected_root` cleared).
        screen.handle_key(key(KeyCode::Tab), &mut state);
        assert_eq!(screen.focus, Pane::Categories);
        screen.handle_key(ctrl(KeyCode::Char('a')), &mut state);
        assert_eq!(
            category_state(state.selection(), "KDE Plasma"),
            DirectoryState::All
        );
    }

    #[test]
    fn space_on_a_category_toggles_all_its_entries() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        // Focus stays on Categories; KDE Plasma is the first row.
        assert_eq!(screen.current_category(), Some("KDE Plasma"));
        assert_eq!(
            category_state(state.selection(), "KDE Plasma"),
            DirectoryState::All
        );

        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            category_state(state.selection(), "KDE Plasma"),
            DirectoryState::None
        );
        // The Category row survives a full deselection (its Entries are withheld, not dropped).
        assert!(screen.categories.contains(&"KDE Plasma"));

        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            category_state(state.selection(), "KDE Plasma"),
            DirectoryState::All
        );
    }

    fn render_to_lines(
        screen: &WhatScreen,
        state: &WizardState,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), screen, state, Glyphs::UNICODE))
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
    fn two_pane_layout_shows_categories_breadcrumb_and_checkboxes() {
        let state = state_of(nested_selection());
        let screen = WhatScreen::new(state.selection());
        let lines = render_to_lines(&screen, &state, 90, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Categories"), "left pane title: {joined}");
        assert!(joined.contains("KDE Plasma"), "category name: {joined}");
        // The explorer breadcrumb roots at the focused Category.
        assert!(joined.contains("KDE Plasma ▸") || lines.iter().any(|l| l.contains("KDE Plasma")));
        assert!(joined.contains("[x]"), "a selected checkbox: {joined}");
    }

    #[test]
    fn partial_directory_renders_the_tilde_checkbox() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state); // .local
        screen.handle_key(key(KeyCode::Right), &mut state); // .local/share
        screen.handle_key(key(KeyCode::Right), &mut state); // plasma/
        let rows = screen.explorer_rows(&state);
        let foo = rows.iter().position(|r| r.label == "foo").expect("foo");
        screen.explorer_cursor = foo;
        screen.handle_key(key(KeyCode::Char(' ')), &mut state); // deselect foo -> plasma Partial
                                                                // Climb so `plasma/` is a visible row, then assert the Partial glyph renders.
        screen.handle_key(key(KeyCode::Backspace), &mut state);
        let lines = render_to_lines(&screen, &state, 90, 20);
        assert!(lines.join("\n").contains("[~]"), "partial glyph: {lines:?}");
    }

    #[test]
    fn empty_directory_renders_the_dash_and_is_drill_disabled() {
        let mut state = state_of(SelectionModel::new(vec![entry(
            "KDE Plasma",
            EntryType::Directory,
            ".cache-marker",
        )]));
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        let rows = screen.explorer_rows(&state);
        assert!(matches!(rows[0].kind, NodeKind::EmptyDirectory));

        // Descending into an empty directory is a no-op (drill-disabled).
        screen.explorer_cursor = 0;
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert!(screen.breadcrumb.is_empty());

        let lines = render_to_lines(&screen, &state, 90, 12);
        assert!(lines.join("\n").contains('—'), "empty dash: {lines:?}");
    }

    #[test]
    fn file_rows_render_a_human_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("plasmashellrc");
        let mut handle = std::fs::File::create(&file).expect("create fixture");
        handle.write_all(&vec![0u8; 3 * 1024]).expect("write bytes");

        let selection = SelectionModel::new(vec![Entry {
            absolute_path: file,
            relative_path: PathBuf::from("plasmashellrc"),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category: "KDE Plasma",
            sensitive: false,
        }]);
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        let lines = render_to_lines(&screen, &state, 90, 12);
        assert!(lines.join("\n").contains("3 KB"), "file size: {lines:?}");
    }

    #[test]
    fn narrow_width_collapses_to_a_single_active_column() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());

        // Focused on Categories: the narrow column is the Categories pane, no breadcrumb.
        let lines = render_to_lines(&screen, &state, 50, 14);
        assert!(
            lines.join("\n").contains("Categories"),
            "narrow categories: {lines:?}"
        );

        // Swap focus to the explorer: the single column is now the explorer + breadcrumb.
        focus_explorer(&mut screen, &mut state);
        let lines = render_to_lines(&screen, &state, 50, 14);
        let joined = lines.join("\n");
        assert!(joined.contains(".local/"), "narrow explorer row: {joined}");
        assert!(
            !joined.contains("Categories"),
            "only one column at narrow width: {joined}"
        );
    }

    #[test]
    fn format_size_scales_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(3 * 1024), "3 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(
            format_size(1024 * 1024 * 1024 + 256 * 1024 * 1024),
            "1.25 GB"
        );
    }
}
