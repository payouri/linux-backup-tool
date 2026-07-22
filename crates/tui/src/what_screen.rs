//! The WHAT step body: the two-pane recursive Drill-down explorer (#48 / #50 / ADR-0006).
//!
//! Left pane = Categories with a selected-Entry count; right pane = a *single-directory*
//! explorer over the currently-focused Category's Entries, with a breadcrumb (a literal `/`
//! separator, #85) pinned above. `Tab`/`Shift-Tab` toggles focus; `→`/`Enter` descends into a
//! directory (the breadcrumb grows, the column reloads with that directory's children) and
//! `Backspace` climbs one level, to any depth (inert at the Category's top level). `←` climbs
//! the same way while drilled in, but at the top level it instead switches focus to Categories
//! (reusing `Tab`'s `toggle_focus`, #85) since there is nothing left to climb out of. `Space`
//! toggles the focused node — a directory toggles its whole subtree (prefix-wins, no
//! carve-outs) — and `Ctrl-A`/`Ctrl-D` select/deselect the whole focused scope. Below
//! [`TWO_PANE_MIN_WIDTH`] the split collapses to a single active column that `Tab` swaps.
//! That focus-toggle meaning is only for the two-pane view: while the Custom-path add field
//! is open, `Tab`/`→`/`End` all accept the highlighted autocomplete candidate instead (#84,
//! see [`WhatScreen::handle_field_key`]).
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
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::model::{DirectoryState, EntryType, SelectionModel};
use linux_backup_core::{catalog, fs_probe, location, Privilege};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::glyphs::Glyphs;
use crate::path_autocomplete::{self, Autocomplete, Getenv};
use crate::reflow::{self, BreadcrumbWidth};
use crate::wizard::WizardState;

/// The always-shown Custom-paths Category (CONTEXT.md's "Custom path", story 6) — its left-pane
/// row is the entry point for the autocomplete add field (#50 / #75).
const CUSTOM_PATHS: &str = catalog::CUSTOM_PATHS.name;

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
    /// row survives for re-selection. The Custom-paths row is always present and pinned last.
    categories: Vec<&'static str>,
    /// The live Custom-path add field (#75). `None` when closed; `Some` while the user is typing
    /// a path into the fish-style autocomplete opened from the Custom-paths row.
    custom_input: Option<CustomInput>,
}

/// The view state of the Custom-path add field — the buffer plus its autocomplete cursor and
/// reveal-hidden toggle. The engine over it is the shared [`crate::path_autocomplete`]; the run
/// facts it needs (`home`, `privilege`, `getenv`) arrive per-call from [`crate::app`] rather than
/// being stored, so this screen holds no ambient run state (ADR-0004).
#[derive(Debug, Clone, Default)]
struct CustomInput {
    /// The literal buffer — `~`/`$VAR` preserved, expansion at the `tui` boundary (ADR-0004).
    buffer: String,
    /// The highlighted candidate; clamped to the live candidate count on move.
    ac_selected: usize,
    /// The reveal-all-dotfiles toggle, flipped by `F3` (#51/#52), mirroring the WHERE field.
    show_hidden: bool,
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
    /// a row. The Custom-paths Category is always present and pinned to the bottom (its row is
    /// the always-shown add affordance, story 6 / #50), regardless of whether it holds anything.
    fn sync_categories(&mut self, selection: &SelectionModel) {
        let mut known: BTreeSet<&'static str> = self.categories.iter().copied().collect();
        for entry in selection.entries() {
            known.insert(entry.category);
        }
        known.remove(CUSTOM_PATHS);
        let mut ordered: Vec<&'static str> = known.into_iter().collect();
        ordered.push(CUSTOM_PATHS);
        self.categories = ordered;
    }

    /// Whether the Custom-path add field is focused — the signal [`crate::app`] uses to route
    /// plain keys (and suppress the `1`–`4` step jump) to the field while it is open (#51).
    #[must_use]
    pub fn is_typing(&self) -> bool {
        self.custom_input.is_some()
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
            // At the Category's top level (nothing to climb) `←` instead switches focus to
            // Categories — reusing `toggle_focus` so neither cursor resets (#85). `Backspace`
            // stays climb-only and is simply inert there; drilled in, both climb as before, so
            // this arm must be checked before the general climb arm below.
            KeyCode::Left if self.focus == Pane::Explorer && self.breadcrumb.is_empty() => {
                self.toggle_focus();
            }
            KeyCode::Left | KeyCode::Backspace if self.focus == Pane::Explorer => self.climb(),
            // On the Categories pane, `→`/`Enter` opens the Custom-path add field when the
            // Custom-paths row is focused (#50), else moves focus into the explorer it drives.
            KeyCode::Right | KeyCode::Enter => {
                if self.current_category() == Some(CUSTOM_PATHS) {
                    self.custom_input = Some(CustomInput::default());
                } else {
                    self.focus = Pane::Explorer;
                }
            }
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

    /// Folds one key into the open Custom-path add field (#75), mirroring the WHERE field's
    /// autocomplete controls: `Tab`/`→`/`End` all accept the highlighted candidate (menu-complete,
    /// #84 — `↓` first to pick a different sibling), `↑`/`↓` move the candidate cursor, `Enter`
    /// accepts a partial or commits the named path, `Esc` closes the field, and
    /// characters/`Backspace` edit the buffer. `App` routes here (with the run facts and env
    /// resolver the wizard reducer doesn't carry, ADR-0004) only while [`Self::is_typing`] holds;
    /// the step-motion chords stay with the wizard.
    pub fn handle_field_key(
        &mut self,
        key: KeyEvent,
        state: &mut WizardState,
        home: &Path,
        privilege: Privilege,
        getenv: Getenv,
    ) {
        if self.custom_input.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.custom_input = None,
            KeyCode::Enter => self.commit_custom_path(state, home, privilege, getenv),
            KeyCode::Tab | KeyCode::Right | KeyCode::End => {
                let view = self.custom_autocomplete(home, privilege, getenv);
                if let Some(input) = self.custom_input.as_mut() {
                    path_autocomplete::accept_ghost(&mut input.buffer, &view);
                    input.ac_selected = 0;
                }
            }
            KeyCode::Up => {
                if let Some(input) = self.custom_input.as_mut() {
                    input.ac_selected = input.ac_selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let view = self.custom_autocomplete(home, privilege, getenv);
                if let Some(input) = self.custom_input.as_mut()
                    && !view.candidates.is_empty()
                {
                    input.ac_selected = (input.ac_selected + 1).min(view.candidates.len() - 1);
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.custom_input.as_mut() {
                    input.buffer.pop();
                    input.ac_selected = 0;
                }
            }
            KeyCode::Char(c) => {
                if let Some(input) = self.custom_input.as_mut() {
                    input.buffer.push(c);
                    input.ac_selected = 0;
                }
            }
            _ => {}
        }
    }

    /// Flips the field's reveal-all-dotfiles toggle (the `F3` reveal, #51). A no-op while the
    /// field is closed, so `App` can call it unconditionally on the WHAT step.
    pub fn toggle_custom_show_hidden(&mut self) {
        if let Some(input) = self.custom_input.as_mut() {
            input.show_hidden = !input.show_hidden;
        }
    }

    /// Seeds the just-opened Custom-path field with its privilege-aware literal prefill — `~/`
    /// non-root, `/` root (#85) — left un-expanded in the buffer (ADR-0004: expansion happens
    /// at the `tui` boundary on commit, never here). The buffer only ever grows/shrinks from
    /// its end (there is no mid-buffer cursor), so this prefill *is* "cursor at end-of-buffer,
    /// autocomplete fires immediately" — no separate cursor state to set. Guarded on an empty
    /// buffer rather than a dedicated "just opened" flag: the field always opens from
    /// [`CustomInput::default`]'s empty buffer, so the first call fills it, while the
    /// step-motion chords (`Ctrl-←`/`Ctrl-→`) — the one plain-key path `App` still routes here
    /// while the field is open — can only ever re-fill a buffer the user has already emptied,
    /// never clobber one they are mid-editing.
    pub fn prime_custom_path(&mut self, privilege: Privilege) {
        if let Some(input) = self.custom_input.as_mut()
            && input.buffer.is_empty()
        {
            input.buffer = match privilege {
                Privilege::Root => "/".to_string(),
                Privilege::NonRoot => "~/".to_string(),
            };
        }
    }

    /// The live candidate listing for the open field, or the empty listing when it is closed.
    fn custom_autocomplete(
        &self,
        home: &Path,
        privilege: Privilege,
        getenv: Getenv,
    ) -> Autocomplete {
        match &self.custom_input {
            Some(input) => path_autocomplete::compute(
                &input.buffer,
                input.ac_selected,
                input.show_hidden,
                home,
                privilege,
                getenv,
            ),
            None => Autocomplete::empty(),
        }
    }

    /// `Enter` in the field: a non-empty fragment whose highlighted candidate would extend it
    /// descends/extends (the candidate list drives `Enter`); otherwise the buffer is committed as
    /// a Custom path. Unlike WHERE (which requires a writable directory), the Selection accepts an
    /// **existing file or directory** — existing-paths-only, the tool never creates — so the
    /// commit expands at the boundary (ADR-0004), enforces the lexical privilege scope
    /// ([`catalog::validate_custom_path`]), confirms the path exists via `core`'s listing
    /// (ADR-0005 D6), then joins it to the Selection through [`SelectionModel::add_custom_path`].
    fn commit_custom_path(
        &mut self,
        state: &mut WizardState,
        home: &Path,
        privilege: Privilege,
        getenv: Getenv,
    ) {
        let Some(input) = self.custom_input.as_ref() else {
            return;
        };
        if input.buffer.trim().is_empty() {
            return;
        }
        let view = self.custom_autocomplete(home, privilege, getenv);
        let (_, fragment) = path_autocomplete::split_buffer(&input.buffer);
        // A partial fragment with a highlighted candidate that would extend it: descend/extend
        // rather than commit, so `Enter` navigates the candidate list like the WHERE field.
        if !fragment.is_empty()
            && let Some(candidate) = view.candidates.get(view.selected)
            && candidate.name.to_string_lossy() != fragment
        {
            if let Some(input) = self.custom_input.as_mut() {
                path_autocomplete::accept_ghost(&mut input.buffer, &view);
                input.ac_selected = 0;
            }
            return;
        }
        let expanded = location::expand_path(&input.buffer, home, getenv);
        if catalog::validate_custom_path(&expanded, home, privilege).is_err() {
            return;
        }
        if !path_exists(&expanded) {
            return;
        }
        state.selection_mut().add_custom_path(&expanded, home);
        self.custom_input = None;
        self.sync_categories(state.selection());
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

/// Whether `path` names an existing file or directory, checked through `core`'s directory listing
/// (ADR-0005 D6) rather than a direct `tui` stat: list its parent and look for the final
/// component. Committing a Custom path is existing-paths-only — the tool never creates — so a
/// path not already present is rejected.
fn path_exists(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    fs_probe::list_dir(parent).is_ok_and(|entries| entries.iter().any(|entry| entry.name == name))
}

/// A left-pane Category row: its name, its count of selected Entries, whether it carries any
/// Sensitive Entry (the lock), its derived tri-state, and whether it is the empty Custom-paths
/// row (which renders the `[+]` add affordance instead of a checkbox, #50).
#[derive(Debug, Clone)]
struct CategoryRow {
    name: &'static str,
    selected: usize,
    sensitive: bool,
    state: DirectoryState,
    add_affordance: bool,
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
                add_affordance: name == CUSTOM_PATHS && all.is_empty(),
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
/// single active column the focused pane fills. While the Custom-path add field is open the
/// explorer column (or the whole narrow width) becomes the autocomplete field (#75); the field
/// needs the injected run facts + env resolver, so `App` threads them in here (ADR-0004/0005 D6).
// The run facts (home/privilege/getenv) join the frame's render inputs so the Custom-path field's
// autocomplete resolves at the `tui` boundary; bundling them would only move the argument list.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &WhatScreen,
    state: &WizardState,
    home: &Path,
    privilege: Privilege,
    getenv: Getenv,
    glyphs: Glyphs,
) {
    let selection = state.selection();
    let categories = category_rows(screen, selection);
    let typing = screen.custom_input.is_some();

    if area.width < TWO_PANE_MIN_WIDTH {
        if typing {
            render_custom_field(frame, area, screen, home, privilege, getenv, glyphs);
        } else {
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
                Pane::Explorer => {
                    let rows = screen.explorer_rows(state);
                    render_explorer(
                        frame,
                        area,
                        &rows,
                        screen.current_category(),
                        &screen.breadcrumb,
                        screen.explorer_cursor,
                        true,
                        glyphs,
                    );
                }
            }
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
        screen.focus == Pane::Categories && !typing,
        glyphs,
    );
    if typing {
        render_custom_field(frame, columns[1], screen, home, privilege, getenv, glyphs);
    } else {
        let rows = screen.explorer_rows(state);
        render_explorer(
            frame,
            columns[1],
            &rows,
            screen.current_category(),
            &screen.breadcrumb,
            screen.explorer_cursor,
            screen.focus == Pane::Explorer,
            glyphs,
        );
    }
}

/// Draws the Custom-path add field into the explorer region: the live buffer with inline
/// ghost-text, the candidate list (dirs-first, dotfiles hidden until a leading `.`), an
/// out-of-scope warning for a non-root path outside `~`, and the standing scope hint — the same
/// fish-style surface as the WHERE field, over the shared engine ([`crate::path_autocomplete`]).
#[allow(clippy::too_many_arguments)]
fn render_custom_field(
    frame: &mut Frame,
    area: Rect,
    screen: &WhatScreen,
    home: &Path,
    privilege: Privilege,
    getenv: Getenv,
    glyphs: Glyphs,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_style(true))
        .title("Add a Custom path");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let Some(input) = &screen.custom_input else {
        return;
    };

    let dim = Style::default().add_modifier(Modifier::DIM);
    let view = screen.custom_autocomplete(home, privilege, getenv);
    let ghost = path_autocomplete::ghost(&input.buffer, &view);

    let mut lines: Vec<Line> = vec![Line::styled("Existing file or directory:".to_string(), dim)];
    let mut field = vec![
        Span::raw(format!("{} [ ", glyphs.pointer)),
        Span::raw(input.buffer.clone()),
    ];
    if !ghost.is_empty() {
        field.push(Span::styled(ghost, dim));
    }
    field.push(Span::raw(" ]"));
    lines.push(Line::from(field));

    lines.extend(path_autocomplete::candidate_lines(
        &view,
        glyphs,
        inner.height as usize,
    ));

    lines.push(Line::styled(
        path_autocomplete::scope_hint(privilege).to_string(),
        dim,
    ));

    let visible: Vec<Line> = lines.into_iter().take(inner.height as usize).collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

/// The breadcrumb pinned above the explorer: the Category root plus every descended component,
/// joined by a literal `/` separator — decoupled from `glyphs.pointer` (#85), which stays the
/// pane-cursor/field-marker glyph. Responsive over the explorer pane's inner `cols` via
/// [`BreadcrumbWidth`]; no leading `/`, since the head segment is always a Category display
/// name, never a path root.
fn breadcrumb_line(category: Option<&str>, crumbs: &[OsString], cols: u16) -> String {
    let category = category.unwrap_or("");
    let segments: Vec<String> = crumbs
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    let tier = BreadcrumbWidth::for_width(cols);
    reflow::render_breadcrumb(tier, category, &segments, cols)
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
            // The empty Custom-paths row shows the `[+]` add affordance rather than a checkbox
            // (#50); once it holds Entries it renders the normal tri-state like any Category.
            let box_glyph = if row.add_affordance {
                "[+]"
            } else {
                checkbox(row.state)
            };
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

// `category`/`crumbs` join `rows` rather than a pre-built breadcrumb string (#85) because the
// text's responsive rung depends on this pane's own inner width, only known once `block.inner`
// runs below.
#[allow(clippy::too_many_arguments)]
fn render_explorer(
    frame: &mut Frame,
    area: Rect,
    rows: &[ExplorerRow],
    category: Option<&str>,
    crumbs: &[OsString],
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
    // Its responsive rung (#85) is keyed off this pane's own inner width, known only here.
    let split = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    let breadcrumb = breadcrumb_line(category, crumbs, inner.width);
    frame.render_widget(
        Paragraph::new(breadcrumb).style(Style::default().add_modifier(Modifier::DIM)),
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// The fixed test home + non-root privilege the Custom-path field tests drive against; the
    /// non-wizard `state_of` roots the Selection under the same home.
    const HOME: &str = "/home/tester";

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
    fn left_at_explorer_top_level_focuses_categories_without_resetting_cursors() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = 1; // Shell, per the alphabetical Category order
        focus_explorer(&mut screen, &mut state);
        assert!(
            screen.breadcrumb.is_empty(),
            "nothing drilled into yet, still top level"
        );
        let explorer_cursor_before = screen.explorer_cursor;

        screen.handle_key(key(KeyCode::Left), &mut state);

        assert_eq!(
            screen.focus,
            Pane::Categories,
            "← at the top level switches focus"
        );
        assert_eq!(
            screen.category_cursor, 1,
            "toggle_focus never touches either cursor"
        );
        assert_eq!(
            screen.explorer_cursor, explorer_cursor_before,
            "explorer cursor is retained for a → back in"
        );
    }

    #[test]
    fn backspace_is_inert_at_the_explorer_top_level() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        assert!(screen.breadcrumb.is_empty());

        screen.handle_key(key(KeyCode::Backspace), &mut state);

        assert_eq!(
            screen.focus,
            Pane::Explorer,
            "Backspace stays climb-only, never switches focus"
        );
        assert!(screen.breadcrumb.is_empty(), "nothing to climb");
    }

    #[test]
    fn left_climbs_one_level_when_drilled_in_like_backspace() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state); // into .local
        assert_eq!(screen.breadcrumb, [OsString::from(".local")]);

        screen.handle_key(key(KeyCode::Left), &mut state);

        assert!(
            screen.breadcrumb.is_empty(),
            "← climbs back out while drilled in, same as Backspace"
        );
        assert_eq!(
            screen.focus,
            Pane::Explorer,
            "climbing stays on the explorer pane"
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
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    screen,
                    state,
                    Path::new(HOME),
                    Privilege::NonRoot,
                    &no_env,
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
    fn two_pane_layout_shows_categories_breadcrumb_and_checkboxes() {
        let state = state_of(nested_selection());
        let screen = WhatScreen::new(state.selection());
        let lines = render_to_lines(&screen, &state, 90, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Categories"), "left pane title: {joined}");
        assert!(joined.contains("KDE Plasma"), "category name: {joined}");
        assert!(joined.contains("[x]"), "a selected checkbox: {joined}");
    }

    #[test]
    fn drilled_in_breadcrumb_uses_a_literal_slash_separator() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.handle_key(key(KeyCode::Right), &mut state); // into .local
        screen.handle_key(key(KeyCode::Enter), &mut state); // into .local/share

        // Wide enough (>= 60 explorer inner cols) to land on the Comfortable rung, which still
        // shows the Category and spaces the `/` separators (#85) — decoupled from `glyphs.pointer`.
        let joined = render_to_lines(&screen, &state, 90, 20).join("\n");
        assert!(
            joined.contains("KDE Plasma / .local / share"),
            "breadcrumb: {joined}"
        );
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

    // --- Custom-path add field (#75) ---------------------------------------------------------

    fn custom_paths_index(screen: &WhatScreen) -> usize {
        screen
            .categories
            .iter()
            .position(|&c| c == CUSTOM_PATHS)
            .expect("the always-shown Custom paths row")
    }

    /// Types `text` into the open field one plain-char key at a time, then folds `finish`.
    fn drive_field(
        screen: &mut WhatScreen,
        state: &mut WizardState,
        home: &Path,
        text: &str,
        finish: KeyCode,
    ) {
        for c in text.chars() {
            screen.handle_field_key(
                key(KeyCode::Char(c)),
                state,
                home,
                Privilege::NonRoot,
                &no_env,
            );
        }
        screen.handle_field_key(key(finish), state, home, Privilege::NonRoot, &no_env);
    }

    fn render_field_lines(
        screen: &WhatScreen,
        state: &WizardState,
        home: &Path,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    screen,
                    state,
                    home,
                    Privilege::NonRoot,
                    &no_env,
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
    fn the_custom_paths_row_is_always_shown_and_pinned_last() {
        // Nothing added yet, but the row exists (story 6) and sits at the bottom of the list.
        let state = state_of(nested_selection());
        let screen = WhatScreen::new(state.selection());
        assert_eq!(screen.categories.last(), Some(&CUSTOM_PATHS));
    }

    #[test]
    fn right_on_the_custom_paths_row_opens_the_add_field() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        assert!(!screen.is_typing());
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert!(
            screen.is_typing(),
            "→ on the Custom paths row opens the field"
        );
        // Focus stays on Categories, not the explorer, while the field is open.
        assert_eq!(screen.focus, Pane::Categories);
    }

    #[test]
    fn right_on_an_ordinary_category_still_moves_into_the_explorer() {
        // The Custom-paths special-case must not change every Category's → behaviour.
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        assert_eq!(screen.current_category(), Some("KDE Plasma"));
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert!(!screen.is_typing());
        assert_eq!(screen.focus, Pane::Explorer);
    }

    #[test]
    fn opening_the_field_non_root_primes_the_tilde_slash_prefill() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);

        screen.prime_custom_path(Privilege::NonRoot);

        assert_eq!(
            screen.custom_input.as_ref().expect("field open").buffer,
            "~/"
        );
    }

    #[test]
    fn opening_the_field_as_root_primes_the_slash_prefill() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);

        screen.prime_custom_path(Privilege::Root);

        assert_eq!(
            screen.custom_input.as_ref().expect("field open").buffer,
            "/"
        );
    }

    #[test]
    fn priming_never_overwrites_a_buffer_the_user_already_edited() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);
        screen.prime_custom_path(Privilege::NonRoot);
        if let Some(input) = screen.custom_input.as_mut() {
            input.buffer = "~/notes".to_string();
        }

        // A second prime (e.g. a stray call on an unrelated key) must not clobber the edit.
        screen.prime_custom_path(Privilege::NonRoot);

        assert_eq!(
            screen.custom_input.as_ref().expect("field open").buffer,
            "~/notes"
        );
    }

    #[test]
    fn priming_a_closed_field_is_a_no_op() {
        let state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        assert!(!screen.is_typing());
        screen.prime_custom_path(Privilege::NonRoot);
        assert!(!screen.is_typing(), "priming never opens the field itself");
    }

    #[test]
    fn commits_an_existing_file_and_an_existing_directory_as_custom_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::write(home.join("notes.md"), b"x").expect("fixture file");
        std::fs::create_dir(home.join("projects")).expect("fixture dir");

        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());

        // An existing *file* joins the Selection as a Custom-paths Entry.
        screen.custom_input = Some(CustomInput::default());
        drive_field(&mut screen, &mut state, home, "~/notes.md", KeyCode::Enter);
        assert!(!screen.is_typing(), "committing closes the field");
        assert!(state
            .selection()
            .entries()
            .iter()
            .any(|e| e.absolute_path == home.join("notes.md")));

        // An existing *directory* is equally valid (unlike WHERE, which requires a writable dir).
        screen.custom_input = Some(CustomInput::default());
        drive_field(&mut screen, &mut state, home, "~/projects", KeyCode::Enter);
        assert!(state
            .selection()
            .entries()
            .iter()
            .any(|e| e.absolute_path == home.join("projects")));

        // Both landed under the Custom paths Category.
        assert_eq!(
            state
                .selection()
                .entries()
                .iter()
                .filter(|e| e.category == CUSTOM_PATHS)
                .count(),
            2
        );
    }

    #[test]
    fn a_nonexistent_path_is_refused_and_the_field_stays_open() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());

        screen.custom_input = Some(CustomInput::default());
        drive_field(&mut screen, &mut state, home, "~/ghost.txt", KeyCode::Enter);
        // The tool never creates a path, so a non-existent one is not added; the field is kept
        // open so the user can correct it.
        assert!(screen.is_typing());
        assert!(state.selection().entries().is_empty());
    }

    #[test]
    fn non_root_scope_blocks_a_path_outside_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.custom_input = Some(CustomInput {
            buffer: "/etc".to_string(),
            ..Default::default()
        });

        // The autocomplete offers nothing out of scope, and a commit is refused lexically.
        let view = screen.custom_autocomplete(home, Privilege::NonRoot, &no_env);
        assert!(view.out_of_scope);
        assert!(view.candidates.is_empty());
        screen.handle_field_key(
            key(KeyCode::Enter),
            &mut state,
            home,
            Privilege::NonRoot,
            &no_env,
        );
        assert!(state.selection().entries().is_empty());
    }

    #[test]
    fn tab_autocompletes_a_path_in_the_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir(home.join("projects")).expect("fixture dir");
        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.custom_input = Some(CustomInput {
            buffer: "~/pro".to_string(),
            ..Default::default()
        });
        screen.handle_field_key(
            key(KeyCode::Tab),
            &mut state,
            home,
            Privilege::NonRoot,
            &no_env,
        );
        assert_eq!(
            screen.custom_input.as_ref().expect("field open").buffer,
            "~/projects/"
        );
    }

    #[test]
    fn tab_accepts_the_highlighted_candidate_even_when_ambiguous() {
        // Same #84 menu-complete behaviour as the WHERE field, over the WHAT Custom-path field
        // (the shared engine's second caller): `Tab` no longer stops at the longest common prefix.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        for dir in ["Documents", "Downloads", "Docker"] {
            std::fs::create_dir(home.join(dir)).expect("fixture dir");
        }
        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.custom_input = Some(CustomInput {
            buffer: "~/D".to_string(),
            ..Default::default()
        });
        screen.handle_field_key(
            key(KeyCode::Tab),
            &mut state,
            home,
            Privilege::NonRoot,
            &no_env,
        );
        assert_eq!(
            screen.custom_input.as_ref().expect("field open").buffer,
            "~/Docker/"
        );
    }

    #[test]
    fn empty_custom_paths_row_renders_the_add_affordance() {
        let state = state_of(nested_selection());
        let screen = WhatScreen::new(state.selection());
        let joined = render_to_lines(&screen, &state, 90, 20).join("\n");
        assert!(
            joined.contains("Custom paths"),
            "always-shown row: {joined}"
        );
        assert!(joined.contains("[+]"), "the add affordance glyph: {joined}");
    }

    #[test]
    fn the_open_field_renders_its_title_buffer_and_candidates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir(home.join("Documents")).expect("fixture dir");
        let state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.custom_input = Some(CustomInput {
            buffer: "~/Doc".to_string(),
            ..Default::default()
        });
        let joined = render_field_lines(&screen, &state, home, 90, 20).join("\n");
        assert!(
            joined.contains("Add a Custom path"),
            "field title: {joined}"
        );
        assert!(joined.contains("~/Doc"), "the live buffer: {joined}");
        assert!(joined.contains("Documents"), "a candidate row: {joined}");
    }
}
