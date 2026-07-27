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
//!
//! **Custom paths (#106/#107/#148) opt out of the recursive explorer above** — its column is a
//! flat, non-drilling list built by [`custom_path_rows`], since a Custom path is always exactly
//! one Entry with no allowlist-derived children to descend into (ADR-0006's dated addendum).
//! `→`/`Enter` there open the add field only when the Category is empty
//! ([`custom_paths_is_empty`], withheld-inclusive so a withheld-only Category still routes to
//! its rows); otherwise, like every other Category, they move focus into the Explorer. `a` adds
//! a Custom path from anywhere in this step; `r` removes the focused Custom-paths row and is
//! silently inert everywhere else (`self.focus != Explorer` or the shown Category isn't Custom
//! paths). Emptying the Category via `r` re-opens the add field and resets focus to
//! `Pane::Categories` so `Esc` lands back on a live row rather than an empty pane.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::model::{DirectoryState, Entry, EntryRoot, EntryType, SelectionModel};
use linux_backup_core::{catalog, fs_probe, location, Privilege};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::glyphs::Glyphs;
use crate::path_autocomplete::{self, Autocomplete, Getenv};
use crate::pending_probe::PendingProbe;
use crate::reflow::{self, BreadcrumbWidth};
use crate::wizard::WizardState;

/// One result a WHAT probe can resolve to — mirrors [`crate::run_screen::RunMessage`]'s shape
/// (ADR-0009 D1): a single channel carries whichever op kind was in flight, so [`WhatScreen`]
/// doesn't need a probe-kind-specific channel per op. No production probe is requested through
/// this yet — which ops ride it is [#96](https://github.com/payouri/linux-backup-tool/issues/96);
/// this ticket lands the spine only. Neither variant is constructed outside this module's own
/// tests yet — see [`WhatScreen::set_pending_probe`].
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatProbeResult {
    /// A single subtree's on-disk size, keyed by its absolute path (a candidate for #96's
    /// per-file `fs_probe::subtree_size` probe).
    Size(PathBuf, u64),
    /// The whole Selection's on-disk size (a candidate for #96's `total_size` probe).
    TotalSize(u64),
}

/// Where a pending WHAT probe's spinner appears once it crosses the debounce threshold, and
/// what value it stands in for until the probe resolves (#94). Recorded alongside the probe at
/// request time — which row a probe is *for* is known up front, before its value is, which is
/// the nature of "pending" — so `render` never has to guess which row a still-in-flight probe
/// belongs to. Neither variant is constructed outside this module's own tests yet, mirroring
/// [`WhatProbeResult`]: no production op requests a probe until #96 wires one in.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeTarget {
    /// An Explorer file row, keyed by its absolute path (matching [`WhatProbeResult::Size`]'s
    /// own key) — the spinner replaces that row's metric-column entry.
    Explorer(PathBuf),
    /// A Categories row, keyed by its Category name — the spinner sits next to that row.
    Category(&'static str),
}

/// The elapsed-time threshold (ADR-0009 D3) past which a pending probe's spinner becomes
/// visible. Below this, WHAT renders exactly as if nothing were pending, so a probe that
/// resolves quickly never flickers anything.
const SPINNER_DEBOUNCE: Duration = Duration::from_millis(375);

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
///
/// Not `Clone`: [`Self::pending_probe`] owns a live worker channel that has no meaningful copy
/// (mirroring [`crate::app::View`]'s own not-`Clone` rationale), so [`Wizard`](crate::wizard::Wizard)
/// — which holds a `WhatScreen` — isn't `Clone` either.
#[derive(Debug, Default)]
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
    /// Memoized `absolute_path -> on-disk size` (#83) — see [`cached_subtree_size`]'s doc for
    /// why this needs interior mutability (`render` only ever holds `&WhatScreen`).
    size_cache: RefCell<HashMap<PathBuf, u64>>,
    /// The single in-flight WHAT probe, if any (ADR-0009 D2), paired with the
    /// [`ProbeTarget`] row its eventual value belongs to: replaced, never queued, when a new
    /// one is requested — see [`WhatScreen::tick`]. `None` outside a pending probe is also
    /// when [`crate::app::App::needs_poll`] stops polling for WHAT. No production request path
    /// exists yet (#96); this ticket lands the debounced-spinner render (#94) against a
    /// synthetic probe, ready for whichever real op #96 wires in first.
    pending_probe: Option<(PendingProbe<WhatProbeResult>, ProbeTarget)>,
    /// Bumped once per poll tick while a probe is pending (mirroring `RunScreen`'s own
    /// `spinner_tick`, `run_screen.rs`), animating the debounced spinner's frame once it
    /// becomes visible (#94).
    spinner_tick: usize,
    /// The cached left-pane Category rows (#112). Computed eagerly at exactly four choke points:
    /// `sync_categories`, `toggle`, `select_scope`, and `commit_custom_path` (#112). This avoids
    /// O(categories × entries) recomputation on every render and folds `category_state`'s prior
    /// double-scan into a single pass.
    category_rows: Vec<CategoryRow>,
    /// Bumped at each of the same four Selection-mutating choke points that recompute
    /// [`Self::category_rows`] — the staleness signal in [`Self::explorer_cache`]'s key. A
    /// counter rather than a `bool` dirty flag so the key is a value comparison with no reset
    /// step to forget, and it can never report "clean" for a generation it hasn't seen.
    selection_generation: u64,
    /// Memoized right-pane explorer rows, keyed by everything that can change them: the
    /// Selection generation above, the focused Category, and the breadcrumb.
    ///
    /// Lazy-with-a-key rather than #112's eager-recompute-at-choke-points shape, because the
    /// explorer rows depend on two things a Selection mutation doesn't cover — the focused
    /// Category and the breadcrumb — whose writers (`set_cursor`, `descend`, `climb`) don't
    /// carry a `&SelectionModel` to recompute from. Putting those inputs *in the key* keeps
    /// every one of them correct without threading the Selection through the cursor writers,
    /// and makes a missed choke point impossible for anything but a Selection edit.
    ///
    /// Interior mutability for the same reason [`Self::size_cache`] needs it: `render` only ever
    /// holds `&WhatScreen` (ADR-0005 D1's pure render), and this is the cache that keeps the
    /// render off the O(Subtree) derivation entirely.
    explorer_cache: RefCell<Option<ExplorerCache>>,
}

/// [`WhatScreen::explorer_cache`]'s contents: the derived rows plus the exact inputs they were
/// derived from. Rows are shared by `Rc` so a hit hands out a clone of the handle, never a copy
/// of a Subtree's worth of rows.
#[derive(Debug)]
struct ExplorerCache {
    generation: u64,
    category: Option<&'static str>,
    breadcrumb: Vec<OsString>,
    rows: Rc<Vec<ExplorerRow>>,
}

/// The view state of the Custom-path add field — the buffer plus its autocomplete cursor and
/// reveal-hidden toggle. The engine over it is the shared [`crate::path_autocomplete`]; the run
/// facts it needs (`home`, `privilege`, `getenv`) arrive per-call from [`crate::app`] rather than
/// being stored, so this screen holds no ambient run state (ADR-0004).
#[derive(Debug, Clone)]
struct CustomInput {
    /// The literal buffer — `~`/`$VAR` preserved, expansion at the `tui` boundary (ADR-0004).
    buffer: String,
    /// The highlighted candidate; clamped to the live candidate count on move.
    ac_selected: usize,
    /// The candidate cap the most recent `render_custom_field` actually displayed (#87) — a
    /// deliberate, narrow exception to `App::update`'s "no size-dependent state" rule and
    /// ADR-0005 D1's pure render, the same shape as this screen's own `size_cache` below. See
    /// `WhereStep::visible_cap`'s twin doc for the full rationale; `handle_field_key`'s `↓`/`↑`
    /// read this instead of a `Rect` they never see.
    visible_cap: Cell<usize>,
    /// The reveal-all-dotfiles toggle, flipped by `F3` (#51/#52), mirroring the WHERE field.
    show_hidden: bool,
}

impl Default for CustomInput {
    /// `visible_cap` starts at `MAX_CANDIDATE_CAP` — today's uncapped ceiling — until the field's
    /// first render narrows it to the real pane height (#87); `#[derive(Default)]` would instead
    /// zero it, wrongly pinning `↓`/`↑` to the first candidate before any render ever runs.
    fn default() -> Self {
        Self {
            buffer: String::new(),
            ac_selected: 0,
            visible_cap: Cell::new(path_autocomplete::MAX_CANDIDATE_CAP),
            show_hidden: false,
        }
    }
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
    /// Recomputes the cached `category_rows` only when the category set actually grows (#112);
    /// see the guard below for why unconditional recompute would defeat the cache.
    fn sync_categories(&mut self, selection: &SelectionModel) {
        let mut known: BTreeSet<&'static str> = self.categories.iter().copied().collect();
        for entry in selection.entries() {
            known.insert(entry.category);
        }
        known.remove(CUSTOM_PATHS);
        let mut ordered: Vec<&'static str> = known.into_iter().collect();
        ordered.push(CUSTOM_PATHS);
        // Categories only ever grow here (never drop, per this fn's contract), so a length
        // change is exactly a growth. Recompute only then: sync_categories runs on every key
        // (top of handle_key), and recomputing unconditionally would make every cursor-only
        // keystroke pay the O(categories × entries) scan this cache exists to avoid (#112).
        let grew = ordered.len() != self.categories.len();
        self.categories = ordered;
        if grew {
            self.recompute_category_rows(selection);
        }
    }

    /// Recomputes the cached `category_rows` from the current categories and selection state
    /// (#112). Called eagerly at four choke points (`sync_categories`, `toggle`, `select_scope`,
    /// `commit_custom_path`) rather than on every render. Folds `category_state`'s prior
    /// double-scan into a single pass over entries.
    ///
    /// Those four are exactly the points where the Selection can have changed under this screen,
    /// so this is also where [`Self::selection_generation`] is bumped — keeping the two caches on
    /// one invalidation path rather than two that can drift apart.
    fn recompute_category_rows(&mut self, selection: &SelectionModel) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.category_rows = self
            .categories
            .iter()
            .map(|&name| {
                let all = selection.entries_in_category(name);
                let sensitive = all.iter().any(|entry| entry.sensitive);

                // Count active entries in this category, folding in the computation that
                // used to be done by `category_state` to avoid the double-scan.
                let active = selection
                    .entries()
                    .iter()
                    .filter(|entry| entry.category == name)
                    .count();
                let total = all.len();

                let state = if active == 0 {
                    DirectoryState::None
                } else if active == total {
                    DirectoryState::All
                } else {
                    DirectoryState::Partial
                };

                CategoryRow {
                    name,
                    selected: active,
                    sensitive,
                    state,
                    add_affordance: name == CUSTOM_PATHS && all.is_empty(),
                }
            })
            .collect();
    }

    /// Whether the Custom-path add field is focused — the signal [`crate::app`] uses to route
    /// plain keys (and suppress the `1`–`4` step jump) to the field while it is open (#51).
    #[must_use]
    pub fn is_typing(&self) -> bool {
        self.custom_input.is_some()
    }

    /// Whether a WHAT probe is currently in flight (ADR-0009 D2) — the signal
    /// [`crate::app::App::needs_poll`] checks to keep the runtime's poll loop alive for WHAT's
    /// own slow ops, on top of the existing backup/restore disjuncts. `false` whenever nothing
    /// is pending, which is also WHAT's steady state today: no production op requests a probe
    /// yet (#96 wires the first one), so outside of a test-injected probe this never joins the
    /// poll loop at all.
    #[must_use]
    pub fn has_pending_probe(&self) -> bool {
        self.pending_probe.is_some()
    }

    /// Advances WHAT's own poll loop by one tick (ADR-0009 D2), mirroring
    /// `drain_backup_worker`/`RunScreen::tick` (`run_screen.rs`): a non-blocking `try_recv` that,
    /// on success, folds the resolved [`WhatProbeResult`] into the relevant cache and clears the
    /// pending slot, plus a `spinner_tick` bump so the debounced spinner (#94) has a live frame
    /// to animate on once it becomes visible. A disconnected channel (an abandoned worker) also
    /// clears the slot — there is nothing further to wait on. A no-op when nothing is pending, so
    /// calling this on every tick regardless of view is harmless (`App::update` does exactly
    /// that).
    pub fn tick(&mut self) {
        let Some((probe, _target)) = &self.pending_probe else {
            return;
        };
        match probe.try_recv() {
            Ok(result) => {
                self.pending_probe = None;
                self.apply_probe_result(result);
            }
            Err(TryRecvError::Empty) => {}
            // The worker panicked, or its send raced a replacement that already cleared this
            // slot — either way there is nothing further to wait on.
            Err(TryRecvError::Disconnected) => self.pending_probe = None,
        }
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    /// The pending probe's target once it's been in flight past [`SPINNER_DEBOUNCE`] (#94) —
    /// `None` both when nothing is pending and while a pending probe is still inside the
    /// debounce window, so `render` shows a spinner in exactly the cases the acceptance criteria
    /// name and never for a probe that resolves quickly.
    fn visible_spinner_target(&self) -> Option<&ProbeTarget> {
        let (probe, target) = self.pending_probe.as_ref()?;
        (probe.elapsed() >= SPINNER_DEBOUNCE).then_some(target)
    }

    /// Folds one resolved [`WhatProbeResult`] into its cache. `TotalSize` has no cache slot yet
    /// (#96 decides where it lives); the variant exists so the channel can already carry it once
    /// a producer lands.
    fn apply_probe_result(&mut self, result: WhatProbeResult) {
        match result {
            WhatProbeResult::Size(path, size) => {
                self.size_cache.borrow_mut().insert(path, size);
            }
            WhatProbeResult::TotalSize(_) => {}
        }
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
            // `a` adds a Custom path from anywhere in the WHAT step (#106/#107/#148) — the
            // app's first bare-letter binding, checked after the `Ctrl-a`/`Ctrl-d` scope-select
            // arms above so it never shadows them.
            KeyCode::Char('a') => self.open_add_field(),
            // `r` removes the focused Custom-paths row; pane-scoped and silently inert
            // everywhere else (checked inside `remove_focused_custom_path`, which states its
            // own scope rather than leaning on `core`'s Category guard two crates away).
            KeyCode::Char('r') => self.remove_focused_custom_path(state),
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
            // On the Categories pane, `→`/`Enter` opens the Custom-path add field only when the
            // Custom-paths Category is empty (#106/#107/#148) — otherwise it moves focus into
            // the explorer, exactly like every other Category, so the rows `r` can act on are
            // actually reachable. `custom_paths_is_empty` is withheld-inclusive and shared with
            // the `[+]` affordance gating in `category_rows`, so the two can't drift.
            KeyCode::Right | KeyCode::Enter => {
                if self.current_category() == Some(CUSTOM_PATHS)
                    && custom_paths_is_empty(state.selection())
                {
                    self.custom_input = Some(CustomInput::default());
                } else {
                    self.focus = Pane::Explorer;
                }
            }
            _ => {}
        }
        self.clamp(state);
    }

    /// `a`: opens the Custom-path add field from wherever the user currently is (#106/#107/
    /// #148) — unlike the Categories-pane `→`/`Enter` special case, this never depends on
    /// which row is focused. Deliberately does not touch `focus`/`breadcrumb`: committing (or
    /// `Esc`-cancelling) leaves the user exactly where `a` was pressed, mid-Drill-down in
    /// whatever Category they were in (`commit_custom_path` already never navigates).
    fn open_add_field(&mut self) {
        self.custom_input = Some(CustomInput::default());
    }

    /// `r`: removes the Entry at the focused Custom-paths row (#106/#107/#148) — pane-scoped,
    /// silently inert unless `self.focus == Pane::Explorer` and the shown Category is Custom
    /// paths, mirroring the `a` special case's own scope guard rather than leaning on `core`'s
    /// Category guard two crates away. No subtree sweep: exactly the focused row's Entry.
    ///
    /// Emptying the Category this way re-opens the add field, focused, and resets the
    /// underlying pane focus to `Categories` — without the reset, `Esc` would close the field
    /// onto the now-empty Explorer, the app's first focusable blank pane where every key
    /// (`↑↓`, `Space`, the scope chords, `r` itself) is inert. With it, `Esc` lands back on the
    /// Custom-paths row showing its `[+]` glyph: exactly the state a fresh wizard opens in.
    fn remove_focused_custom_path(&mut self, state: &mut WizardState) {
        if self.focus != Pane::Explorer || self.current_category() != Some(CUSTOM_PATHS) {
            return;
        }
        let rows = self.explorer_rows(state);
        let Some(row) = rows.get(self.explorer_cursor) else {
            return;
        };
        state.selection_mut().remove_custom_path(&row.node_path);
        self.sync_categories(state.selection());
        if custom_paths_is_empty(state.selection()) {
            self.custom_input = Some(CustomInput::default());
            self.focus = Pane::Categories;
        }
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
            Pane::Categories => {
                if self.category_cursor != index {
                    self.breadcrumb.clear();
                    self.explorer_cursor = 0;
                }
                self.category_cursor = index;
            }
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
    /// Triggers a recompute of the cached `category_rows` after the mutation (#112).
    fn toggle(&mut self, state: &mut WizardState) {
        match self.focus {
            Pane::Categories => {
                if let Some(name) = self.current_category() {
                    // Use cached category_rows to determine the state before the mutation,
                    // avoiding a separate category_state() call (#112).
                    let select = self
                        .category_rows
                        .iter()
                        .find(|row| row.name == name)
                        .is_none_or(|row| matches!(row.state, DirectoryState::None));
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
        self.recompute_category_rows(state.selection());
    }

    /// `Ctrl-A`/`Ctrl-D`: select or deselect the whole focused scope — every Category on the
    /// left, or every row of the current directory on the right (#51 Select group).
    /// Triggers a recompute of the cached `category_rows` after the loop (#112).
    fn select_scope(&mut self, state: &mut WizardState, select: bool) {
        match self.focus {
            Pane::Categories => {
                for name in self.categories.clone() {
                    set_category(state, name, select);
                }
            }
            Pane::Explorer => {
                // A snapshot of the rows as they stand before the loop: `set_node` mutates the
                // Selection the rows were derived from, so iterating the shared handle (rather
                // than re-deriving per row) is both what keeps this one pass and what keeps it
                // reading the same list the user saw.
                let rows = self.explorer_rows(state);
                for row in rows.iter() {
                    set_node(state, row, select);
                }
            }
        }
        self.recompute_category_rows(state.selection());
    }

    /// The rows of the directory the explorer currently shows, derived from the Selection (no
    /// stored tree) and memoized in [`Self::explorer_cache`].
    ///
    /// Custom paths (#106/#107/#148) deliberately opt out of the recursive grouped explorer:
    /// it renders its own flat, non-drilling list instead, since a Custom path is always
    /// exactly one Entry with no allowlist-derived children. The memoization is the same
    /// either way — a flat list is derived from the Selection under the same key.
    ///
    /// The derivation is `O(log n + k)` in the breadcrumb's own Subtree (#83), but `k` is the
    /// whole Category at a Category's top level and 20k+ in a real wide directory, and a single
    /// keypress used to run it three times over (`active_len`, `clamp`, then `render`). Cursor
    /// motion changes none of this function's inputs, so on a movement key every call after the
    /// first is a key comparison; only a Selection edit, a Category switch, or a drill-down
    /// re-derives.
    fn explorer_rows(&self, state: &WizardState) -> Rc<Vec<ExplorerRow>> {
        let category = self.current_category();
        if let Some(cache) = self.explorer_cache.borrow().as_ref()
            && cache.generation == self.selection_generation
            && cache.category == category
            && cache.breadcrumb == self.breadcrumb
        {
            return Rc::clone(&cache.rows);
        }
        let rows = Rc::new(match category {
            Some(CUSTOM_PATHS) => custom_path_rows(state.selection(), &self.size_cache),
            Some(category) => explorer_rows(
                state.selection(),
                category,
                &self.breadcrumb,
                &self.size_cache,
            ),
            None => Vec::new(),
        });
        *self.explorer_cache.borrow_mut() = Some(ExplorerCache {
            generation: self.selection_generation,
            category,
            breadcrumb: self.breadcrumb.clone(),
            rows: Rc::clone(&rows),
        });
        rows
    }

    /// Keeps both cursors within their (possibly shrunk) row counts after any action.
    fn clamp(&mut self, state: &WizardState) {
        let categories = self.categories.len();
        if categories > 0 {
            self.category_cursor = self.category_cursor.min(categories - 1);
        } else {
            self.category_cursor = 0;
        }
        // A cursor already at 0 is in range for *every* row count — `min(0, len - 1)` is 0, and
        // the empty-pane branch sets 0 — so the clamp below is a provable no-op there. Skipping
        // it skips the row derivation it would need, which matters because a Categories-pane
        // cursor move resets `explorer_cursor` to 0 (#111) and would otherwise re-derive the
        // newly focused Category's whole listing inside the reducer, on every keystroke, purely
        // to confirm that 0 is still a valid index.
        if self.explorer_cursor == 0 {
            return;
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
                    input.ac_selected = path_autocomplete::clamp_to_visible(
                        input.ac_selected + 1,
                        view.candidates.len(),
                        input.visible_cap.get(),
                    );
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
                input.visible_cap.get(),
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
        // A committed custom path lands under the always-present CUSTOM_PATHS category, so it
        // changes that row's counts without growing the category set — sync_categories' growth
        // guard won't fire. Recompute explicitly: this is the fourth deliberate-intent choke
        // point (#112).
        self.sync_categories(state.selection());
        self.recompute_category_rows(state.selection());
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

/// Whether the Custom-paths Category holds no Entry at all — **withheld-inclusive** (reads
/// `entries_in_category`, not the active-only Selection), which is load-bearing (#106/#107/
/// #148): a Category holding only withheld Custom paths must count as non-empty, so `→` lands
/// on rows `r` can still act on rather than opening the add field and putting them permanently
/// out of reach. Shared by the Categories-pane `→`/`Enter` arm and the `[+]` affordance gating
/// below, so the field-opens condition and the affordance advertising it cannot drift apart.
fn custom_paths_is_empty(selection: &SelectionModel) -> bool {
    selection.entries_in_category(CUSTOM_PATHS).is_empty()
}

/// The tri-state of a whole Category, from its active-vs-total Entry split — the left pane's
/// analogue of a directory node's [`SelectionModel::directory_state`].
#[cfg(test)]
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

/// Walks each candidate path's ancestors to find which deselected roots cover it.
/// This mirrors `core::SelectionModel::is_under_deselected_root` but inverted: instead of
/// checking if a single path is under any deselected root, it finds all deselected roots that
/// cover at least one path in the candidate set.
///
/// This is O(paths × depth × log m) instead of the naive O(roots × paths), avoiding the freeze
/// when re-enabling a large Category that was deselected leaf-by-leaf (creating O(k) `deselected_roots`).
fn find_deselected_roots_covering_paths(
    paths: &[PathBuf],
    deselected_roots: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for path in paths {
        for ancestor in path.ancestors() {
            if ancestor == Path::new("") {
                break;
            }
            if deselected_roots.contains(ancestor) {
                roots.insert(ancestor.to_path_buf());
            }
        }
    }
    roots.into_iter().collect()
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
        let covering_roots: Vec<PathBuf> =
            find_deselected_roots_covering_paths(&paths, selection.deselected_roots());
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
    Directory {
        children: usize,
    },
    EmptyDirectory,
    File {
        size: u64,
    },
    /// A directory Custom path (#106/#107/#148) — real, non-empty on disk, but its size is
    /// unknown here: `fs_probe::subtree_size` doesn't walk (#147), so the flat Custom-paths
    /// list can only show `—`, same as [`Self::EmptyDirectory`]'s glyph but *not* the same
    /// meaning, so this stays its own variant rather than reusing that one — reusing it is
    /// what caused the bug this variant fixes for free: a Custom-path directory used to render
    /// as `EmptyDirectory` (dimmed, "no children") because the has-children signal only ever
    /// came from same-Category Entries, which a Custom directory Entry never has.
    DirectoryUnknownSize,
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
    /// The underlying Entry's absolute path, for a file row only — `None` for a directory or
    /// empty directory, which have no single Entry (and so no probe) to key against. Matched
    /// against a pending [`ProbeTarget::Explorer`] to decide whether this row's metric column
    /// shows the debounced spinner instead of [`Self::metric`] (#94).
    absolute_path: Option<PathBuf>,
    kind: NodeKind,
    state: DirectoryState,
}

impl ExplorerRow {
    /// The right-hand metric column: child count, size, or the empty-directory dash.
    fn metric(&self) -> String {
        match &self.kind {
            NodeKind::Directory { children } => format!("{children} items"),
            NodeKind::EmptyDirectory | NodeKind::DirectoryUnknownSize => "—".to_string(),
            NodeKind::File { size } => format_size(*size),
        }
    }

    fn is_file(&self) -> bool {
        matches!(self.kind, NodeKind::File { .. })
    }
}

/// Accumulates, per immediate child component under the current prefix, whether it has deeper
/// Entries (a real directory) and — if an Entry sits exactly at it — that Entry's own facts.
/// `any_active`/`any_withheld` are folded in alongside the grouping itself (#83) so a row's
/// [`DirectoryState`] falls out of the same pass that groups it, rather than a second per-row
/// query against the model — mirrors `directory_state`'s own `(any_active, any_withheld)` read.
///
/// The tri-state flags are set **across every Category** under the node (matching
/// `directory_state`, which is Category-agnostic), while `in_category` / the kind fields stay
/// scoped to the explorer's focused Category: a directory node — e.g. the top-level `.config` or
/// `.local` row shared by KDE Plasma, Program configs, and Cloud creds — toggles its whole
/// Subtree regardless of Category (`deselect_root` is Category-agnostic too), so its checkbox
/// must reflect every Entry it would touch, not just the focused Category's share. A row is only
/// emitted when `in_category` (the focused Category has at least one Entry under it).
#[derive(Default)]
struct ChildAccumulator {
    has_children: bool,
    grandchildren: BTreeSet<OsString>,
    exact: Option<(PathBuf, EntryType)>,
    in_category: bool,
    any_active: bool,
    any_withheld: bool,
}

impl ChildAccumulator {
    fn state(&self) -> DirectoryState {
        match (self.any_active, self.any_withheld) {
            (true, false) => DirectoryState::All,
            (true, true) => DirectoryState::Partial,
            (false, _) => DirectoryState::None,
        }
    }
}

/// Folds one Entry, known to be under `prefix`, into its row's [`ChildAccumulator`] — shared by
/// both the active and withheld passes below, `active` recording which one this Entry came from.
/// The `any_active`/`any_withheld` tri-state flags accrue from every Category (so a row's state
/// matches `directory_state`); the row's existence and kind accrue only from `category`.
fn accumulate_child(
    groups: &mut BTreeMap<OsString, ChildAccumulator>,
    prefix: &Path,
    entry: &Entry,
    category: &str,
    active: bool,
) {
    let Ok(rest) = entry.relative_path.strip_prefix(prefix) else {
        return;
    };
    let mut components = rest.components();
    let Some(first) = components.next() else {
        return; // the directory node itself, not one of its children
    };
    let accumulator = groups.entry(first.as_os_str().to_os_string()).or_default();
    if active {
        accumulator.any_active = true;
    } else {
        accumulator.any_withheld = true;
    }
    if entry.category != category {
        return;
    }
    accumulator.in_category = true;
    if let Some(second) = components.next() {
        accumulator.has_children = true;
        accumulator
            .grandchildren
            .insert(second.as_os_str().to_os_string());
    } else {
        accumulator.exact = Some((entry.absolute_path.clone(), entry.entry_type));
    }
}

/// Groups the current breadcrumb's Entries into the rows of the directory at `breadcrumb` — the
/// presentation tree ADR-0006 has `tui` derive, with no fresh filesystem reads for the grouping
/// itself (file sizes go through `size_cache`, a memoized wrapper over `core`'s probe, #83).
///
/// #83: sourced from [`SelectionModel::active_entries_under`]/`withheld_entries_under` — a
/// Subtree range scan bounded to `k`, the Entries actually under `breadcrumb` — rather than
/// `entries_in_category`, which scans every Entry in the Selection regardless of Category or
/// breadcrumb. Nothing outside `breadcrumb`'s own Subtree is ever visited. This is also what
/// removes the old per-row `directory_state` call: `ChildAccumulator::state` derives the same
/// tri-state from the `any_active`/`any_withheld` flags this pass sets — accrued across every
/// Category under a node (so the derived state matches Category-agnostic `directory_state`
/// exactly), while a row's existence and kind stay scoped to the focused `category`.
fn explorer_rows(
    selection: &SelectionModel,
    category: &str,
    breadcrumb: &[OsString],
    size_cache: &RefCell<HashMap<PathBuf, u64>>,
) -> Vec<ExplorerRow> {
    let prefix: PathBuf = breadcrumb.iter().collect();
    let mut groups: BTreeMap<OsString, ChildAccumulator> = BTreeMap::new();

    for entry in selection.active_entries_under(&prefix) {
        accumulate_child(&mut groups, &prefix, entry, category, true);
    }
    for entry in selection.withheld_entries_under(&prefix) {
        accumulate_child(&mut groups, &prefix, entry, category, false);
    }

    let mut rows: Vec<ExplorerRow> = groups
        .into_iter()
        .filter(|(_, accumulator)| accumulator.in_category)
        .map(|(component, accumulator)| {
            let node_path = prefix.join(&component);
            let state = accumulator.state();
            let name = component.to_string_lossy();
            let mut absolute_path = None;
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
                    Some((absolute, _)) => {
                        absolute_path = Some(absolute.clone());
                        (
                            NodeKind::File {
                                size: cached_subtree_size(size_cache, &absolute),
                            },
                            name.into_owned(),
                        )
                    }
                    None => (NodeKind::EmptyDirectory, format!("{name}/")),
                }
            };
            ExplorerRow {
                label,
                component,
                node_path,
                absolute_path,
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

/// The Custom-paths Explorer column (#106/#107/#148): a flat list of every added Custom path,
/// active and withheld — no drill-down, no grouping by path component. A Custom path is always
/// exactly one Entry with no allowlist-derived children of its own, so the recursive
/// [`explorer_rows`] machinery (grouping, breadcrumb depth, `→`/`Enter` descent) has nothing to
/// do here; ADR-0006's dated addendum records this as a deliberate opt-out, not an oversight.
/// Withheld Entries stay visible (unchecked) alongside active ones, matching #106's "`r` must
/// work on a withheld path" requirement.
fn custom_path_rows(
    selection: &SelectionModel,
    size_cache: &RefCell<HashMap<PathBuf, u64>>,
) -> Vec<ExplorerRow> {
    let root = PathBuf::new();
    // Both maps are walked because a withheld Custom path stays listed (unchecked) — but each
    // row's tri-state comes from `directory_state`, not from which map it was found in. That
    // matters for a directory Custom path sharing its prefix with another Category: `Space` on
    // it calls the Category-agnostic `deselect_root`, so its checkbox must answer for every
    // Entry that would touch, exactly as the grouped column's does (ADR-0006's #86 addendum).
    let mut rows: Vec<ExplorerRow> = selection
        .active_entries_under(&root)
        .chain(selection.withheld_entries_under(&root))
        .filter(|entry| entry.category == CUSTOM_PATHS)
        .map(|entry| custom_path_row(selection, entry, size_cache))
        .collect();
    // Sorted on the rendered label (#107 decision 10): root-scoped (`/`) rows group ahead of
    // home-scoped (`~/`) ones for free (ASCII `/` < `~`), and the order is always explainable
    // from what's on screen. The dirs-first split the grouped explorer uses carries no meaning
    // in a list of hand-picked paths, so it is deliberately dropped here.
    rows.sort_by_key(|row| row.label.to_lowercase());
    rows
}

/// Builds one flat Custom-paths row from `entry`: a tilde-collapsed (Home root) or `/`-rooted
/// (Absolute root) full path, directories keeping a trailing `/` — derived purely from the
/// Entry's own `root`/`relative_path` (#107), so no `home` needs threading into this builder
/// (ADR-0004's no-ambient-reads boundary stays intact). This round-trips with what the user
/// typed (the add field prefills `~/` non-root, `/` root) and stays honest about an Absolute
/// root: a bare `relative_path` would render `/etc/hosts` as `etc/hosts`, indistinguishable from
/// a Home-rooted `~/etc/hosts`.
///
/// The checkbox comes from [`SelectionModel::directory_state`] — the same Category-agnostic
/// source the grouped column uses (ADR-0006's #86 addendum) — rather than from whether the Entry
/// was found active or withheld: a directory Custom path over another Category's Entries reads
/// `[~]`, which is what `Space`'s Category-agnostic `deselect_root` would actually act on.
fn custom_path_row(
    selection: &SelectionModel,
    entry: &Entry,
    size_cache: &RefCell<HashMap<PathBuf, u64>>,
) -> ExplorerRow {
    let root_prefix = match entry.root {
        EntryRoot::Home => "~/",
        EntryRoot::Absolute => "/",
    };
    let is_directory = entry.entry_type == EntryType::Directory;
    let mut label = format!("{root_prefix}{}", entry.relative_path.display());
    if is_directory {
        label.push('/');
    }
    let kind = if is_directory {
        // Real directory sizes are out of scope (#147: `fs_probe::subtree_size` doesn't walk) —
        // this also fixes an existing bug for free: a directory Custom path used to render as
        // `EmptyDirectory` because the has-children signal only ever came from same-Category
        // Entries.
        NodeKind::DirectoryUnknownSize
    } else {
        NodeKind::File {
            size: cached_subtree_size(size_cache, &entry.absolute_path),
        }
    };
    ExplorerRow {
        label,
        component: entry.relative_path.as_os_str().to_os_string(),
        node_path: entry.relative_path.clone(),
        // File rows only, matching the grouped explorer's rule (#94): a directory Custom path
        // renders as `DirectoryUnknownSize` and so has no probe to key a spinner against.
        absolute_path: (!is_directory).then(|| entry.absolute_path.clone()),
        kind,
        state: selection.directory_state(&entry.relative_path),
    }
}

/// A memoized wrapper over `fs_probe::subtree_size` (#83): a file row's size is `stat`'d once
/// and reused for the life of the [`WhatScreen`] it belongs to, rather than re-`stat`'d on every
/// redraw/keypress `explorer_rows` runs for. `render` takes `&WhatScreen` (ADR-0005 D1's pure
/// render), so the cache needs interior mutability to populate itself from a shared reference.
/// Keyed by absolute path and never evicted: a file's on-disk size is treated as a stable fact
/// for the running WHAT step (the wizard doesn't watch the filesystem), and a fresh wizard run
/// starts from a fresh, empty [`WhatScreen`] (and so a fresh cache) rather than carrying stale
/// sizes across runs.
fn cached_subtree_size(cache: &RefCell<HashMap<PathBuf, u64>>, absolute_path: &Path) -> u64 {
    if let Some(&size) = cache.borrow().get(absolute_path) {
        return size;
    }
    let size = fs_probe::subtree_size(std::iter::once(absolute_path));
    cache.borrow_mut().insert(absolute_path.to_path_buf(), size);
    size
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
        NodeKind::Directory { .. } | NodeKind::EmptyDirectory | NodeKind::DirectoryUnknownSize => {
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
    let typing = screen.custom_input.is_some();
    let (category_spinner, explorer_spinner) = spinner_targets(screen, glyphs);

    if area.width < TWO_PANE_MIN_WIDTH {
        if typing {
            render_custom_field(frame, area, screen, home, privilege, getenv, glyphs);
        } else {
            match screen.focus {
                Pane::Categories => {
                    render_categories(
                        frame,
                        area,
                        &screen.category_rows,
                        screen.category_cursor,
                        true,
                        glyphs,
                        category_spinner,
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
                        explorer_spinner,
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
        &screen.category_rows,
        screen.category_cursor,
        screen.focus == Pane::Categories && !typing,
        glyphs,
        category_spinner,
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
            explorer_spinner,
        );
    }
}

/// A pending probe's render target inside the Categories pane: the Category name it names,
/// paired with the debounce-frame glyph drawn in place of that row's count (#94).
type CategorySpinner<'a> = Option<(&'a str, char)>;
/// A pending probe's render target inside the Explorer pane: the file row's absolute path,
/// paired with the debounce-frame glyph drawn in place of that row's metric (#94).
type ExplorerSpinner<'a> = Option<(&'a Path, char)>;

/// Resolves `screen`'s pending probe (if past the debounce threshold) into the pane-scoped
/// spinner render inputs (#94): at most one of the two is ever `Some`, since a probe targets
/// exactly one pane. Each carries the debounce-frame glyph paired with the key `render_categories`
/// / `render_explorer` match a row against, so a probe outside its own pane's row list is
/// silently drawn nowhere rather than guessed at.
fn spinner_targets(
    screen: &WhatScreen,
    glyphs: Glyphs,
) -> (CategorySpinner<'_>, ExplorerSpinner<'_>) {
    let Some(target) = screen.visible_spinner_target() else {
        return (None, None);
    };
    let glyph = glyphs.spinner[screen.spinner_tick % glyphs.spinner.len()];
    match target {
        ProbeTarget::Category(name) => (Some((*name, glyph)), None),
        ProbeTarget::Explorer(path) => (None, Some((path.as_path(), glyph))),
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

    // Cache the real, currently-known display cap (#87) before computing the view, so
    // `custom_autocomplete`'s `compute` call — and any `↓`/`↑` press after this frame — clamp the
    // highlight to what this same render is about to show.
    input
        .visible_cap
        .set(path_autocomplete::candidate_cap(inner.height as usize));

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

/// `spinner`, when `Some`, pairs the debounce-frame glyph with the Category name a pending probe
/// targets (#94): that row's count is replaced by the bare glyph on the row it names, in place —
/// no dedicated status line, no other row affected.
fn render_categories(
    frame: &mut Frame,
    area: Rect,
    categories: &[CategoryRow],
    cursor: usize,
    focused: bool,
    glyphs: Glyphs,
    spinner: CategorySpinner,
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
            // A debounced probe pending for this row replaces its count with the bare spinner
            // glyph — instant pop-in back to the count on the next render once it resolves,
            // never a fade or highlight (#94).
            let count = match spinner {
                Some((target, glyph)) if target == name => glyph.to_string(),
                _ => row.selected.to_string(),
            };
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
// runs below. `spinner`, when `Some`, pairs the debounce-frame glyph with the absolute path of
// the file row a pending probe targets (#94): that row's metric column shows the bare glyph in
// place of `ExplorerRow::metric`, instant pop-in back to the resolved value on the next render.
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
    spinner: ExplorerSpinner,
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

    // Format only the rows this pane can actually show, rather than formatting all of them and
    // letting `Paragraph::scroll` discard the rest: a real wide directory is 20k+ rows against a
    // viewport of a few dozen, and the discarded 99% cost a `format!` each, every frame. Slicing
    // here is what makes the per-frame cost the viewport's size instead of the Subtree's — so the
    // scroll is applied by the slice, and the widget itself renders unscrolled.
    let offset = scroll_offset(split[1].height, cursor) as usize;
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(split[1].height as usize)
        .map(|(index, row)| {
            let marker = if focused && index == cursor {
                glyphs.pointer
            } else {
                " "
            };
            let box_glyph = checkbox(row.state);
            let label = &row.label;
            let metric = match spinner {
                Some((target, glyph)) if Some(target) == row.absolute_path.as_deref() => {
                    glyph.to_string()
                }
                _ => row.metric(),
            };
            let line = Line::from(format!("{marker}{box_glyph} {label}  {metric}"));
            if matches!(row.kind, NodeKind::EmptyDirectory) {
                line.style(Style::default().add_modifier(Modifier::DIM))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), split[1]);
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
impl WhatScreen {
    /// Test-only: injects a probe and its render target directly into the pending slot,
    /// standing in for the request path a real WHAT op will call once one is wired through
    /// (#96). Lets other modules' tests (`app.rs`'s `needs_poll`/`Tick` coverage, via
    /// [`crate::wizard::Wizard::what_mut`]) set up a pending-probe scenario without a
    /// production trigger to hang it off yet.
    pub(crate) fn set_pending_probe(
        &mut self,
        probe: PendingProbe<WhatProbeResult>,
        target: ProbeTarget,
    ) {
        self.pending_probe = Some((probe, target));
    }
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
    fn switching_categories_resets_breadcrumb_and_explorer_cursor() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        // Drill into KDE Plasma (cursor 0, as it sorts first alphabetically).
        screen.handle_key(key(KeyCode::Right), &mut state); // into .local
        screen.handle_key(key(KeyCode::Right), &mut state); // into .local/share
        assert_eq!(
            screen.breadcrumb,
            [OsString::from(".local"), OsString::from("share")]
        );
        // Move explorer cursor to a non-zero position to verify it resets.
        screen.explorer_cursor = 1;

        // Switch focus to Categories and move to the second Category (Shell).
        screen.handle_key(key(KeyCode::Tab), &mut state); // back to Categories
        assert_eq!(screen.focus, Pane::Categories);
        screen.handle_key(key(KeyCode::Down), &mut state); // to Shell

        // Breadcrumb and explorer cursor must reset when category changes.
        assert!(
            screen.breadcrumb.is_empty(),
            "breadcrumb must clear on category switch"
        );
        assert_eq!(
            screen.explorer_cursor, 0,
            "explorer cursor must reset on category switch"
        );
        assert_eq!(
            screen.category_cursor, 1,
            "category cursor has moved to Shell"
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

    // --- Debounced spinner placement, copy, and dismissal (#94) ------------------------------

    /// A probe that blocks on a release signal until the test sends it, so it stays reliably
    /// "pending" for as long as a debounce-threshold assertion needs — no race against the
    /// worker thread's own scheduling, mirroring `a_new_pending_probe_replaces_rather_than_...`
    /// above.
    fn blocking_probe(
        result: WhatProbeResult,
    ) -> (PendingProbe<WhatProbeResult>, std::sync::mpsc::Sender<()>) {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let probe = PendingProbe::spawn(move || {
            let _ = release_rx.recv();
            result
        });
        (probe, release_tx)
    }

    /// A single-file KDE Plasma Selection rooted at a real 3 KB temp file, so its Explorer row
    /// has a genuine on-disk size for a spinner to stand in for and a resolved value to pop back
    /// to. The `TempDir` must outlive the file's use, hence it is returned alongside the path.
    fn file_backed_selection() -> (tempfile::TempDir, PathBuf, SelectionModel) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("plasmashellrc");
        let mut handle = std::fs::File::create(&file).expect("create fixture");
        handle.write_all(&vec![0u8; 3 * 1024]).expect("write bytes");
        let selection = SelectionModel::new(vec![Entry {
            absolute_path: file.clone(),
            relative_path: PathBuf::from("plasmashellrc"),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category: "KDE Plasma",
            sensitive: false,
        }]);
        (dir, file, selection)
    }

    #[test]
    fn explorer_probe_shows_no_spinner_before_the_debounce_threshold() {
        let (_dir, file, selection) = file_backed_selection();
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        let (probe, release_tx) = blocking_probe(WhatProbeResult::Size(file.clone(), 999_999));
        screen.set_pending_probe(probe, ProbeTarget::Explorer(file));

        // Rendered right away: nowhere near the 375ms debounce threshold yet.
        let joined = render_to_lines(&screen, &state, 90, 12).join("\n");
        assert!(
            joined.contains("3 KB"),
            "expected the real size below the debounce threshold, not a spinner: {joined}"
        );
        assert!(
            Glyphs::UNICODE.spinner.iter().all(|g| !joined.contains(*g)),
            "no spinner glyph should appear before 375ms: {joined}"
        );

        let _ = release_tx.send(());
        wait_for_drain(&mut screen);
    }

    #[test]
    fn explorer_probe_shows_the_bare_spinner_glyph_in_the_metric_column_past_the_threshold() {
        let (_dir, file, selection) = file_backed_selection();
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        let (probe, release_tx) = blocking_probe(WhatProbeResult::Size(file.clone(), 999_999));
        screen.set_pending_probe(probe, ProbeTarget::Explorer(file));
        std::thread::sleep(Duration::from_millis(450));

        let joined = render_to_lines(&screen, &state, 90, 12).join("\n");
        assert!(
            !joined.contains("3 KB"),
            "the metric column must not show the real size while the spinner is up: {joined}"
        );
        assert!(
            Glyphs::UNICODE.spinner.iter().any(|g| joined.contains(*g)),
            "expected a bare spinner glyph in the Explorer row's metric column: {joined}"
        );

        let _ = release_tx.send(());
        wait_for_drain(&mut screen);
    }

    #[test]
    fn categories_probe_shows_the_bare_spinner_glyph_next_to_the_affected_row_past_the_threshold() {
        let state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());

        let (probe, release_tx) = blocking_probe(WhatProbeResult::TotalSize(0));
        screen.set_pending_probe(probe, ProbeTarget::Category("KDE Plasma"));
        std::thread::sleep(Duration::from_millis(450));

        let lines = render_to_lines(&screen, &state, 90, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("KDE Plasma"))
            .expect("the KDE Plasma Categories row");
        assert!(
            Glyphs::UNICODE.spinner.iter().any(|g| row.contains(*g)),
            "expected a bare spinner glyph next to the affected Category row: {row}"
        );

        let _ = release_tx.send(());
        wait_for_drain(&mut screen);
    }

    #[test]
    fn resolved_value_replaces_the_spinner_on_the_very_next_render_with_no_transition() {
        let (_dir, file, selection) = file_backed_selection();
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        // Resolves to the file's own real size: once applied, the cache and the on-disk fact
        // agree, so the post-resolve assertion below is checking dismissal, not a stray value.
        let (probe, release_tx) = blocking_probe(WhatProbeResult::Size(file.clone(), 3 * 1024));
        screen.set_pending_probe(probe, ProbeTarget::Explorer(file));
        std::thread::sleep(Duration::from_millis(450));

        let joined = render_to_lines(&screen, &state, 90, 12).join("\n");
        assert!(
            Glyphs::UNICODE.spinner.iter().any(|g| joined.contains(*g)),
            "expected the spinner up before the probe resolves: {joined}"
        );

        let _ = release_tx.send(());
        wait_for_drain(&mut screen);

        // The very next render after the probe resolves: the resolved value replaces the
        // spinner outright, with no intermediate/transition frame in between (#94).
        let joined = render_to_lines(&screen, &state, 90, 12).join("\n");
        assert!(
            joined.contains("3 KB"),
            "expected the resolved size to replace the spinner immediately: {joined}"
        );
        assert!(
            Glyphs::UNICODE.spinner.iter().all(|g| !joined.contains(*g)),
            "no spinner glyph should remain once the probe has resolved: {joined}"
        );
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
        // #106/#107/#148 re-scope: → unconditionally opening the field was only ever right for
        // an *empty* Custom-paths Category (the story this fixture exercises — `nested_selection`
        // has none). A non-empty row's behavior is pinned separately below.
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        assert!(!screen.is_typing());
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert!(
            screen.is_typing(),
            "→ on the empty Custom paths row opens the field"
        );
        // Focus stays on Categories, not the explorer, while the field is open.
        assert_eq!(screen.focus, Pane::Categories);
    }

    #[test]
    fn right_on_a_non_empty_custom_paths_row_moves_focus_into_the_explorer() {
        let mut selection = nested_selection();
        selection.add_custom_path(Path::new("/home/tester/notes.md"), Path::new(HOME));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);

        screen.handle_key(key(KeyCode::Right), &mut state);

        assert!(
            !screen.is_typing(),
            "→ on a non-empty Custom paths row must not open the add field"
        );
        assert_eq!(screen.focus, Pane::Explorer);
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

    // --- Custom-paths remove/add verbs (#106/#107/#148) --------------------------------------

    #[test]
    fn r_removes_the_focused_custom_path_row() {
        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(Path::new("/home/tester/keep.md"), Path::new(HOME));
        selection.add_custom_path(Path::new("/home/tester/forget.md"), Path::new(HOME));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert_eq!(screen.focus, Pane::Explorer);
        let before = screen.explorer_rows(&state);
        assert_eq!(before.len(), 2);
        let removed_label = before[screen.explorer_cursor].label.clone();

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        let after = screen.explorer_rows(&state);
        assert_eq!(after.len(), 1);
        assert!(!after.iter().any(|r| r.label == removed_label));
        assert_eq!(
            screen.focus,
            Pane::Explorer,
            "removal must not reset focus while the Category still holds a path"
        );
    }

    #[test]
    fn r_is_inert_on_the_categories_pane() {
        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(Path::new("/home/tester/notes.md"), Path::new(HOME));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        assert_eq!(screen.focus, Pane::Categories);

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        assert_eq!(
            state.selection().entries_in_category(CUSTOM_PATHS).len(),
            1,
            "r must no-op while focus is on the Categories pane"
        );
    }

    #[test]
    fn r_is_inert_on_another_categorys_explorer_row() {
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state); // KDE Plasma row focused in the Explorer

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        assert_eq!(
            state.selection().entries().len(),
            4,
            "r must no-op on any Category other than Custom paths"
        );
    }

    #[test]
    fn r_removes_a_withheld_custom_path_row() {
        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(Path::new("/home/tester/keep.md"), Path::new(HOME));
        selection.add_custom_path(Path::new("/home/tester/notes.md"), Path::new(HOME));
        selection.deselect_entry(Path::new("notes.md"));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);
        let rows = screen.explorer_rows(&state);
        let withheld_index = rows
            .iter()
            .position(|r| r.label.contains("notes.md"))
            .expect("the withheld row is still listed");
        assert_eq!(
            rows[withheld_index].state,
            DirectoryState::None,
            "a withheld Custom path is listed unchecked, not hidden"
        );
        screen.explorer_cursor = withheld_index;

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        assert!(state
            .selection()
            .entries_in_category(CUSTOM_PATHS)
            .iter()
            .all(|e| e.relative_path != Path::new("notes.md")));
    }

    #[test]
    fn cursor_holds_index_after_r_and_the_next_row_slides_under_it() {
        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(Path::new("/home/tester/a.md"), Path::new(HOME));
        selection.add_custom_path(Path::new("/home/tester/b.md"), Path::new(HOME));
        selection.add_custom_path(Path::new("/home/tester/c.md"), Path::new(HOME));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);
        let before = screen.explorer_rows(&state);
        assert_eq!(before.len(), 3);
        screen.explorer_cursor = 0;
        let second_label = before[1].label.clone();

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        let after = screen.explorer_rows(&state);
        assert_eq!(after.len(), 2);
        assert_eq!(screen.explorer_cursor, 0, "the cursor holds its index");
        assert_eq!(
            after[0].label, second_label,
            "the next row slides under the held cursor"
        );
    }

    #[test]
    fn removing_the_last_custom_path_opens_the_add_field_and_resets_focus_to_categories() {
        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(Path::new("/home/tester/only.md"), Path::new(HOME));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);
        assert_eq!(screen.focus, Pane::Explorer);

        screen.handle_key(key(KeyCode::Char('r')), &mut state);

        assert!(
            screen.is_typing(),
            "removing the last Custom path re-opens the add field"
        );
        assert_eq!(screen.focus, Pane::Categories);

        // Esc from the auto-opened field must land back on the Custom-paths row, not an empty
        // Explorer pane where every key is inert.
        screen.handle_field_key(
            key(KeyCode::Esc),
            &mut state,
            Path::new(HOME),
            Privilege::NonRoot,
            &no_env,
        );
        assert!(!screen.is_typing());
        assert_eq!(screen.focus, Pane::Categories);
        assert_eq!(screen.category_cursor, custom_paths_index(&screen));
    }

    #[test]
    fn a_opens_the_add_field_from_another_category_and_commit_does_not_navigate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::write(home.join("notes.md"), b"x").expect("fixture file");
        let mut state = WizardState::new(nested_selection(), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state); // KDE Plasma explorer focused
        let breadcrumb_before = screen.breadcrumb.clone();

        screen.handle_key(key(KeyCode::Char('a')), &mut state);
        assert!(screen.is_typing(), "a opens the field from anywhere");
        assert_eq!(
            screen.focus,
            Pane::Explorer,
            "opening the field must not move focus"
        );

        drive_field(&mut screen, &mut state, home, "~/notes.md", KeyCode::Enter);

        assert!(!screen.is_typing(), "committing closes the field");
        assert_eq!(screen.focus, Pane::Explorer, "committing must not navigate");
        assert_eq!(screen.breadcrumb, breadcrumb_before);
        assert!(state
            .selection()
            .entries()
            .iter()
            .any(|e| e.absolute_path == home.join("notes.md")));
    }

    #[test]
    fn custom_paths_flat_list_reads_tilde_collapsed_sorted_with_directory_dash_metric() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let other_root = tempfile::tempdir().expect("tempdir");

        let file_path = home.join("notes.md");
        std::fs::write(&file_path, b"0123456789").expect("write fixture file");
        let dir_path = home.join("projects");
        std::fs::create_dir(&dir_path).expect("fixture dir");
        let absolute_path = other_root.path().join("elsewhere.txt");
        std::fs::write(&absolute_path, b"x").expect("write fixture file");

        let mut selection = SelectionModel::new(Vec::new());
        selection.add_custom_path(&file_path, home);
        selection.add_custom_path(&dir_path, home);
        selection.add_custom_path(&absolute_path, home);

        let mut state = WizardState::new(selection, Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);

        let rows = screen.explorer_rows(&state);
        assert_eq!(rows.len(), 3);

        // Root-scoped (`/`) rows sort ahead of home-scoped (`~/`) ones for free (ASCII `/` < `~`).
        assert!(
            rows[0].label.starts_with('/'),
            "expected the Absolute-rooted row first: {:?}",
            rows.iter().map(|r| &r.label).collect::<Vec<_>>()
        );
        let home_rows: Vec<&ExplorerRow> =
            rows.iter().filter(|r| r.label.starts_with('~')).collect();
        assert_eq!(home_rows.len(), 2);
        assert_eq!(home_rows[0].label, "~/notes.md");
        assert_eq!(home_rows[1].label, "~/projects/");

        let notes_row = rows
            .iter()
            .find(|r| r.label == "~/notes.md")
            .expect("file row present");
        assert_eq!(notes_row.metric(), "10 B");
        let projects_row = rows
            .iter()
            .find(|r| r.label == "~/projects/")
            .expect("directory row present");
        assert_eq!(
            projects_row.metric(),
            "—",
            "a directory Custom path shows an unknown-size dash, not an empty-directory dash"
        );
    }

    #[test]
    fn a_directory_custom_path_row_reads_partial_over_another_categorys_withheld_entries() {
        // ADR-0006's #86 addendum: an explorer directory row's tri-state is Category-agnostic,
        // because `Space` on it calls the Category-agnostic `deselect_root`. A `~/.config` Custom
        // path sitting over a withheld Program configs Entry must therefore read `[~]` — a `[x]`
        // there claims a state the toggle would not produce, which is the same dishonesty #86
        // fixed for the grouped column.
        let mut selection = SelectionModel::new(vec![
            entry(CUSTOM_PATHS, EntryType::Directory, ".config"),
            entry("Program configs", EntryType::File, ".config/foo.conf"),
        ]);
        selection.deselect_entry(Path::new(".config/foo.conf"));
        let mut state = state_of(selection);
        let mut screen = WhatScreen::new(state.selection());
        screen.category_cursor = custom_paths_index(&screen);
        screen.handle_key(key(KeyCode::Right), &mut state);

        let rows = screen.explorer_rows(&state);
        assert_eq!(rows.len(), 1, "one row per Custom path, never its children");
        assert_eq!(rows[0].label, "~/.config/");
        assert_eq!(
            rows[0].state,
            DirectoryState::Partial,
            "the row's checkbox must reflect every Entry `Space` would withhold, not just the \
             Custom-paths Category's share"
        );
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

    #[test]
    fn down_cannot_select_a_candidate_past_the_short_pane_display_cap() {
        // #87: same clamp as the WHERE field's twin regression test, over the shared engine's
        // second caller — a short pane's adaptive cap must keep `↓` from advancing the highlight
        // onto a row `candidate_lines` won't draw.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        for i in 0..(path_autocomplete::MAX_CANDIDATE_CAP + 5) {
            std::fs::create_dir(home.join(format!("dir{i:02}"))).expect("fixture dir");
        }
        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        screen.custom_input = Some(CustomInput {
            buffer: "~/".to_string(),
            ..Default::default()
        });
        // Prime the display-cap cache the way the real redraw-on-event loop would: a render
        // always precedes the next key (runtime.rs's `run_loop`).
        render_field_lines(&screen, &state, home, 90, 14);
        for _ in 0..(path_autocomplete::MAX_CANDIDATE_CAP + 5) {
            screen.handle_field_key(
                key(KeyCode::Down),
                &mut state,
                home,
                Privilege::NonRoot,
                &no_env,
            );
        }
        let lines = render_field_lines(&screen, &state, home, 90, 14);
        let joined = lines.join("\n");
        assert_eq!(
            joined.matches('←').count(),
            1,
            "exactly one visible row must carry the highlight cursor: {joined}"
        );
    }

    #[test]
    fn explorer_rows_stays_fast_in_a_wide_directory_with_an_unrelated_huge_sibling() {
        // Regression guard for #83's real-world trigger: a directory with 20k+ items directly
        // in it (`~/.local/share/icons/Breeze KDE-Story-Dark/actions`), rendered once every
        // keypress/redraw. A second, equally huge *sibling* subtree (another icon theme) proves
        // the render is O(visible), not O(whole Category): the old `entries_in_category` +
        // `strip_prefix` scan touched every Entry in the Selection regardless of breadcrumb, so
        // this would have scaled with the sibling's size too, not just the 20,000 rows actually
        // produced.
        const WIDE: usize = 20_000;
        let mut entries: Vec<Entry> = (0..WIDE)
            .map(|i| {
                entry(
                    "KDE Plasma",
                    EntryType::File,
                    &format!(".local/share/icons/theme/actions/icon{i}.svg"),
                )
            })
            .collect();
        entries.extend((0..WIDE).map(|i| {
            entry(
                "KDE Plasma",
                EntryType::File,
                &format!(".local/share/icons/sibling-theme/icon{i}.svg"),
            )
        }));
        let mut state = state_of(SelectionModel::new(entries));
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        screen.breadcrumb = vec![
            OsString::from(".local"),
            OsString::from("share"),
            OsString::from("icons"),
            OsString::from("theme"),
            OsString::from("actions"),
        ];

        let started = std::time::Instant::now();
        let rows = screen.explorer_rows(&state);
        let elapsed = started.elapsed();

        assert_eq!(rows.len(), WIDE, "one row per file directly in `actions/`");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "explorer_rows took {elapsed:?} for a {WIDE}-item directory with an equally large \
             unrelated sibling subtree — the O(whole-Category)-per-render regression is back"
        );
    }

    #[test]
    fn explorer_rows_size_cache_avoids_restatting_an_unchanged_file_on_every_render() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file_path = tmp.path().join("kwinrc");
        std::fs::write(&file_path, b"0123456789").expect("write fixture file");
        let e = Entry {
            absolute_path: file_path.clone(),
            relative_path: PathBuf::from("kwinrc"),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category: "KDE Plasma",
            sensitive: false,
        };
        let mut state = state_of(SelectionModel::new(vec![e]));
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        let first = screen.explorer_rows(&state);
        assert_eq!(first[0].metric(), "10 B");

        // Shrink the file on disk without touching the Selection or the breadcrumb — a render
        // reusing the cached size (not re-`stat`ing) must keep reporting the original size.
        std::fs::write(&file_path, b"x").expect("shrink fixture file");

        let second = screen.explorer_rows(&state);
        assert_eq!(
            second[0].metric(),
            "10 B",
            "the cached size must survive an on-disk change until the cache is rebuilt"
        );
    }

    #[test]
    fn explorer_row_state_matches_directory_state_across_a_shared_cross_category_prefix() {
        // #83 AC: explorer display is unchanged versus today. A directory node the focused
        // Category shares with another (`.local`, holding KDE Plasma's `icons/` and Program
        // configs' `fonts/`) toggles its whole Subtree Category-agnostically, so its checkbox
        // must reflect every Entry under it — not just the focused Category's share. Here a
        // Program-configs sibling is withheld while KDE Plasma's own Entries stay active: the
        // row must read `[~]` (Partial), exactly as the source-of-truth `directory_state` does,
        // not `[x]` (All) derived from the focused Category alone.
        let mut selection = SelectionModel::new(vec![
            entry("KDE Plasma", EntryType::File, ".local/share/icons/a.svg"),
            entry(
                "Program configs",
                EntryType::File,
                ".local/share/fonts/x.ttf",
            ),
        ]);
        selection.deselect_entry(Path::new(".local/share/fonts/x.ttf"));
        let cache = RefCell::new(HashMap::new());

        let rows = explorer_rows(&selection, "KDE Plasma", &[], &cache);
        let dotlocal = rows
            .iter()
            .find(|row| row.component == ".local")
            .expect("the shared `.local` row is present in the KDE Plasma explorer");

        assert_eq!(
            dotlocal.state,
            selection.directory_state(Path::new(".local")),
            "the row's tri-state must equal directory_state's Category-agnostic verdict"
        );
        assert_eq!(dotlocal.state, DirectoryState::Partial);
    }

    #[test]
    fn find_deselected_roots_covering_paths_pinned_against_synthetic_btreeset() {
        // Unit test for the O(paths × depth) ancestor-walk helper. Verifies it correctly
        // identifies which deselected roots (subtree prefixes) cover a candidate set of paths.
        let mut deselected_roots = BTreeSet::new();
        deselected_roots.insert(PathBuf::from(".local/share/icons"));
        deselected_roots.insert(PathBuf::from(".config/plasma"));
        deselected_roots.insert(PathBuf::from(".bashrc"));

        // Single path covered by a root at depth 3.
        let covering = find_deselected_roots_covering_paths(
            &[PathBuf::from(".local/share/icons/hicolor/scalable/app.svg")],
            &deselected_roots,
        );
        assert_eq!(covering.len(), 1);
        assert!(covering.contains(&PathBuf::from(".local/share/icons")));

        // Multiple paths covered by different roots.
        let covering = find_deselected_roots_covering_paths(
            &[
                PathBuf::from(".local/share/icons/a.svg"),
                PathBuf::from(".config/plasma/plasmarc"),
            ],
            &deselected_roots,
        );
        assert_eq!(covering.len(), 2);
        assert!(covering.contains(&PathBuf::from(".local/share/icons")));
        assert!(covering.contains(&PathBuf::from(".config/plasma")));

        // Exact-match root (a file deselection, not a subtree).
        let covering =
            find_deselected_roots_covering_paths(&[PathBuf::from(".bashrc")], &deselected_roots);
        assert_eq!(covering.len(), 1);
        assert!(covering.contains(&PathBuf::from(".bashrc")));

        // Path not covered by any root.
        let covering =
            find_deselected_roots_covering_paths(&[PathBuf::from(".zshrc")], &deselected_roots);
        assert!(covering.is_empty());

        // Multiple paths but one root covers them (no duplication in result).
        let covering = find_deselected_roots_covering_paths(
            &[
                PathBuf::from(".local/share/icons/a.svg"),
                PathBuf::from(".local/share/icons/b.svg"),
            ],
            &deselected_roots,
        );
        assert_eq!(covering.len(), 1);
        assert!(covering.contains(&PathBuf::from(".local/share/icons")));
    }

    #[test]
    fn re_enabling_a_large_category_stays_fast() {
        // Regression test for issue #110: re-enabling a ~40,000-entry Category that was
        // deselected leaf-by-leaf (creating ~40,000 single-file deselected_roots) must
        // complete quickly without UI freeze. The fix extracts a O(paths × depth × log m)
        // ancestor-walk helper, replacing the naive O(roots × paths) scan.
        let leaf_paths: Vec<PathBuf> = (0..40_000)
            .map(|i| PathBuf::from(format!("icon{i}.svg")))
            .collect();

        // Build a synthetic deselected_roots set mimicking 40k leaf deselections.
        let deselected_roots: BTreeSet<PathBuf> = leaf_paths.iter().cloned().collect();

        let started = std::time::Instant::now();
        let covering = find_deselected_roots_covering_paths(&leaf_paths, &deselected_roots);
        let elapsed = started.elapsed();

        // All 40k paths should be found as covering their own exact roots.
        assert_eq!(covering.len(), 40_000);
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "finding covering roots for 40,000 paths took {elapsed:?} — the O(roots × paths) \
             freeze (#110) is back"
        );
    }

    #[test]
    fn has_pending_probe_defaults_to_false() {
        // The steady state (ADR-0009 D2): no production op requests a probe yet, so a freshly
        // built screen never joins the poll loop.
        let screen = WhatScreen::default();
        assert!(!screen.has_pending_probe());
    }

    #[test]
    fn tick_drains_a_resolved_probe_into_the_size_cache_and_clears_the_slot() {
        let mut screen = WhatScreen::default();
        let path = PathBuf::from("/home/tester/big-file");
        screen.set_pending_probe(
            PendingProbe::spawn({
                let path = path.clone();
                move || WhatProbeResult::Size(path, 4096)
            }),
            ProbeTarget::Explorer(path.clone()),
        );
        assert!(screen.has_pending_probe());

        wait_for_drain(&mut screen);

        assert!(
            !screen.has_pending_probe(),
            "expected the resolved probe to clear the pending slot"
        );
        assert_eq!(screen.size_cache.borrow().get(&path), Some(&4096));
    }

    #[test]
    fn tick_is_a_no_op_without_a_pending_probe() {
        let mut screen = WhatScreen::default();
        let spinner_before = screen.spinner_tick;
        screen.tick();
        assert!(!screen.has_pending_probe());
        assert_eq!(
            screen.spinner_tick, spinner_before,
            "the spinner must not advance outside a pending probe"
        );
    }

    #[test]
    fn a_new_pending_probe_replaces_rather_than_queues_the_old_one() {
        let mut screen = WhatScreen::default();
        let stale_path = PathBuf::from("/home/tester/stale");
        let fresh_path = PathBuf::from("/home/tester/fresh");

        // The stale probe blocks until released, so it is still unresolved (guaranteed "in
        // flight") at the moment it is replaced below.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        screen.set_pending_probe(
            PendingProbe::spawn({
                let stale_path = stale_path.clone();
                move || {
                    let _ = release_rx.recv();
                    WhatProbeResult::Size(stale_path, 111)
                }
            }),
            ProbeTarget::Explorer(stale_path.clone()),
        );
        // Replacing the slot drops the stale `PendingProbe` — and its `Receiver` — outright;
        // there is no queue holding both requests in flight (ADR-0009 D2).
        screen.set_pending_probe(
            PendingProbe::spawn({
                let fresh_path = fresh_path.clone();
                move || WhatProbeResult::Size(fresh_path, 222)
            }),
            ProbeTarget::Explorer(fresh_path.clone()),
        );
        // Let the abandoned stale worker finish and attempt its now-nowhere `send`.
        let _ = release_tx.send(());

        wait_for_drain(&mut screen);

        assert!(!screen.has_pending_probe());
        assert_eq!(
            screen.size_cache.borrow().get(&fresh_path),
            Some(&222),
            "expected the replacement probe's result to land in the cache"
        );
        assert_eq!(
            screen.size_cache.borrow().get(&stale_path),
            None,
            "the replaced probe's result must never surface"
        );
    }

    /// Ticks `screen` until its pending probe drains or a bounded deadline passes — avoids a
    /// fixed `sleep` racing the worker thread's own scheduling.
    fn wait_for_drain(screen: &mut WhatScreen) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while screen.has_pending_probe() && std::time::Instant::now() < deadline {
            screen.tick();
            std::thread::yield_now();
        }
    }

    #[test]
    fn cursor_only_keys_leave_cached_rows_untouched_no_recompute() {
        // #112 AC: A cursor-only key (motion, focus toggle, narrowly non-mutating) should not
        // trigger a recompute of the cached category_rows. This test verifies that the cached
        // rows pointer identity stays the same after cursor-only keys.
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        assert_eq!(screen.category_rows.len(), 3); // KDE Plasma, Shell, Custom paths

        // Poke a sentinel into the cache. A recompute would overwrite it with the true count;
        // its survival across the burst is a direct, allocator-independent witness that no
        // recompute ran (pointer identity would be defeated by the allocator reusing the freed
        // buffer, so it can't distinguish "not recomputed" from "recomputed in place").
        for row in &mut screen.category_rows {
            row.selected = usize::MAX;
        }

        // Apply cursor-only keys: arrows, Tab, Home, End, PgUp/PgDn.
        screen.handle_key(key(KeyCode::Up), &mut state);
        screen.handle_key(key(KeyCode::Down), &mut state);
        screen.handle_key(key(KeyCode::PageUp), &mut state);
        screen.handle_key(key(KeyCode::PageDown), &mut state);
        screen.handle_key(key(KeyCode::Home), &mut state);
        screen.handle_key(key(KeyCode::End), &mut state);
        screen.handle_key(key(KeyCode::Tab), &mut state);
        screen.handle_key(key(KeyCode::BackTab), &mut state);

        assert!(
            screen
                .category_rows
                .iter()
                .all(|row| row.selected == usize::MAX),
            "cursor-only keys must not recompute the cached category_rows"
        );
    }

    #[test]
    fn category_cache_recomputes_at_four_choke_points() {
        // #112 AC: The cached category_rows must recompute at exactly four choke points:
        // 1. sync_categories (on category growth)
        // 2. toggle (after mutating branch)
        // 3. select_scope (after loop)
        // 4. commit_custom_path (asserted separately below, as it needs a real filesystem path)
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());

        // Choke point 1: sync_categories (called during construction, so verify it initialized)
        assert_eq!(screen.category_rows.len(), 3); // KDE Plasma, Shell, Custom paths
        assert_eq!(screen.category_rows[0].name, "KDE Plasma");
        assert_eq!(screen.category_rows[0].selected, 3); // All 3 KDE Plasma entries

        // Choke point 2: toggle (Categories pane)
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        // After toggling KDE Plasma off, its selected count should drop to 0.
        assert_ne!(
            screen.category_rows[0].selected, 3,
            "toggle should trigger a recompute"
        );

        // Choke point 3: select_scope (Ctrl-A on Categories)
        screen.handle_key(ctrl(KeyCode::Char('a')), &mut state);
        // After Ctrl-A, all categories should be fully selected again.
        assert_eq!(
            screen.category_rows[0].selected, 3,
            "select_scope should trigger a recompute"
        );

        // Verify the cache is correctly updated by checking all rows.
        let total_selected: usize = screen.category_rows.iter().map(|r| r.selected).sum();
        assert_eq!(
            total_selected, 4,
            "all entries should be selected after Ctrl-A"
        ); // 3 KDE Plasma + 1 Shell
    }

    #[test]
    fn commit_custom_path_recomputes_the_cached_rows() {
        // #112 AC (fourth choke point): a committed custom path lands under the always-present
        // Custom-paths category, so it changes that row's counts without growing the category
        // set. sync_categories' growth guard won't fire, so commit_custom_path must recompute on
        // its own — this asserts the cached row reflects the new Entry.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::write(home.join("notes.md"), b"x").expect("fixture file");

        let mut state = WizardState::new(SelectionModel::new(vec![]), Some(home.to_path_buf()));
        let mut screen = WhatScreen::new(state.selection());
        let before = screen
            .category_rows
            .iter()
            .find(|r| r.name == CUSTOM_PATHS)
            .map(|r| r.selected)
            .expect("the always-shown Custom paths row");
        assert_eq!(before, 0, "no custom path committed yet");

        screen.custom_input = Some(CustomInput::default());
        drive_field(&mut screen, &mut state, home, "~/notes.md", KeyCode::Enter);

        let after = screen
            .category_rows
            .iter()
            .find(|r| r.name == CUSTOM_PATHS)
            .map(|r| r.selected)
            .expect("the always-shown Custom paths row");
        assert_eq!(
            after, 1,
            "commit_custom_path must recompute the cached rows to reflect the new Entry"
        );
    }

    #[test]
    fn movement_bursts_stay_cheap_regardless_of_selection_size() {
        // #112 AC: A burst of movement keys (cursor-only, no mutation) should scale linearly
        // with the number of keys, not with the Selection size. This test generates a large
        // Selection and verifies that repeated cursor movements don't cause repeated recomputes.
        let large_selection = SelectionModel::new(
            (0..100)
                .flat_map(|i| {
                    vec![
                        entry(
                            "KDE Plasma",
                            EntryType::File,
                            &format!(".local/plasma/file{i}"),
                        ),
                        entry("Shell", EntryType::File, &format!(".config/shell{i}")),
                    ]
                })
                .collect(),
        );

        let mut state = WizardState::new(large_selection, Some(PathBuf::from("/home/tester")));
        let mut screen = WhatScreen::new(state.selection());

        // Poke a sentinel into the cache; a recompute on any burst key would overwrite it with
        // the true count. Its survival is an allocator-independent witness that the whole burst
        // did zero recompute work — the cost of a movement key is O(1), not O(Selection size).
        for row in &mut screen.category_rows {
            row.selected = usize::MAX;
        }

        // Apply a burst of movement keys.
        for _ in 0..50 {
            screen.handle_key(key(KeyCode::Down), &mut state);
            screen.handle_key(key(KeyCode::Up), &mut state);
        }

        assert!(
            screen
                .category_rows
                .iter()
                .all(|row| row.selected == usize::MAX),
            "a movement burst must not recompute category_rows even on a large Selection"
        );
    }

    /// A Category whose top level holds one directory that in turn holds `WIDE` files — the real
    /// `~/.local/share/icons/<theme>/actions` shape, plus an equally large unrelated sibling.
    fn wide_directory_selection(wide: usize) -> SelectionModel {
        let mut entries: Vec<Entry> = (0..wide)
            .map(|i| {
                entry(
                    "KDE Plasma",
                    EntryType::File,
                    &format!(".local/share/icons/theme/actions/icon{i}.svg"),
                )
            })
            .collect();
        entries.extend((0..wide).map(|i| {
            entry(
                "KDE Plasma",
                EntryType::File,
                &format!(".local/share/icons/sibling-theme/icon{i}.svg"),
            )
        }));
        SelectionModel::new(entries)
    }

    fn into_wide_leaf(screen: &mut WhatScreen, state: &mut WizardState) {
        focus_explorer(screen, state);
        screen.breadcrumb = vec![
            OsString::from(".local"),
            OsString::from("share"),
            OsString::from("icons"),
            OsString::from("theme"),
            OsString::from("actions"),
        ];
    }

    #[test]
    fn cursor_motion_in_a_wide_directory_never_re_derives_the_explorer_rows() {
        // The reported symptom: with a real large Selection the Explorer cursor lagged badly
        // behind the keypress. A single arrow used to derive the whole directory listing three
        // times over (`active_len`, `clamp`, then `render`), each an O(Subtree) pass — 20k rows
        // here. Cursor motion changes none of the rows' inputs, so after the first derivation a
        // movement burst must be answered entirely from the memo.
        const WIDE: usize = 20_000;
        let mut state = state_of(wide_directory_selection(WIDE));
        let mut screen = WhatScreen::new(state.selection());
        into_wide_leaf(&mut screen, &mut state);

        // Prime the memo, then swap the derived rows for a sentinel only a re-derivation could
        // erase — the same observable-staleness trick `explorer_rows_size_cache_avoids_restatting
        // _an_unchanged_file_on_every_render` uses on the size cache.
        assert_eq!(screen.explorer_rows(&state).len(), WIDE);
        if let Some(cache) = screen.explorer_cache.borrow_mut().as_mut() {
            cache.rows = Rc::new(Vec::new());
        }

        for _ in 0..200 {
            screen.handle_key(key(KeyCode::Down), &mut state);
        }
        for _ in 0..200 {
            screen.handle_key(key(KeyCode::Up), &mut state);
        }

        assert!(
            screen.explorer_rows(&state).is_empty(),
            "a 400-key movement burst re-derived the listing — the explorer memo is not holding \
             across cursor motion, which is exactly the per-keystroke O(Subtree) cost this fixes"
        );
    }

    #[test]
    fn the_explorer_memo_reflects_a_selection_edit_a_category_switch_and_a_drill_down() {
        // The memo is only safe if every input that changes the rows is in its key. One test per
        // key dimension, asserting through the rows themselves rather than the cache internals.
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);

        // Drill-down: the breadcrumb is part of the key.
        let top = screen.explorer_rows(&state);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].label, ".local/");
        screen.handle_key(key(KeyCode::Right), &mut state);
        let descended = screen.explorer_rows(&state);
        assert_eq!(descended[0].label, "share/", "descent must re-derive");
        screen.handle_key(key(KeyCode::Left), &mut state);
        assert_eq!(
            screen.explorer_rows(&state)[0].label,
            ".local/",
            "climbing back must re-derive"
        );

        // Selection edit: the generation is part of the key.
        let before = screen.explorer_rows(&state)[0].state;
        assert_eq!(before, DirectoryState::All);
        screen.handle_key(key(KeyCode::Char(' ')), &mut state);
        assert_eq!(
            screen.explorer_rows(&state)[0].state,
            DirectoryState::None,
            "a Space toggle must be visible in the very next derivation"
        );

        // Category switch: the focused Category is part of the key.
        screen.handle_key(key(KeyCode::Tab), &mut state);
        screen.handle_key(key(KeyCode::Down), &mut state);
        assert_eq!(screen.current_category(), Some("Shell"));
        assert_eq!(
            screen.explorer_rows(&state)[0].label,
            ".zshrc",
            "switching Category must re-derive against the new Category"
        );
    }

    #[test]
    fn a_scrolled_explorer_draws_the_window_around_its_cursor() {
        // The render formats only the visible slice rather than every row. The window it picks
        // must still be the one holding the cursor, with the cursor marker on the right row.
        const WIDE: usize = 20_000;
        let mut state = state_of(wide_directory_selection(WIDE));
        let mut screen = WhatScreen::new(state.selection());
        into_wide_leaf(&mut screen, &mut state);
        for _ in 0..500 {
            screen.handle_key(key(KeyCode::Down), &mut state);
        }
        assert_eq!(screen.explorer_cursor, 500);

        // Rows sort case-insensitively by label, so index 500 is whatever that ordering puts
        // there — read it off the listing rather than assuming a numeric sequence.
        let rows = screen.explorer_rows(&state);
        let focused = rows[500].label.clone();
        let first = rows[0].label.clone();

        let lines = render_to_lines(&screen, &state, 120, 20);
        let joined = lines.join("\n");
        let cursor_row = lines
            .iter()
            .find(|line| line.contains(Glyphs::UNICODE.pointer))
            .expect("the focused row is drawn inside the visible window");
        assert!(
            cursor_row.contains(&focused),
            "the cursor marker must sit on row 500 ({focused}), got: {cursor_row}"
        );
        assert!(
            !joined.contains(&first),
            "row 0 ({first}) is scrolled far out of a 20-row viewport and must not be drawn: \
             {joined}"
        );
    }

    #[test]
    fn clamp_still_pulls_a_non_zero_out_of_range_explorer_cursor_back_into_range() {
        // `clamp` now skips its row derivation when the cursor is already 0 — provably in range
        // for every row count, so the derivation only ever confirmed what arithmetic already
        // guarantees. The guard must not cost the clamp its actual job on a *non-zero* cursor,
        // which is the only case it was ever load-bearing for.
        let mut state = state_of(nested_selection());
        let mut screen = WhatScreen::new(state.selection());
        focus_explorer(&mut screen, &mut state);
        // KDE Plasma's top level is the single `.local/` row, so anything above 0 is out of
        // range. Park the cursor there directly — no production path leaves it there today, and
        // the point is that clamp corrects it regardless of how it got there.
        screen.explorer_cursor = 7;

        screen.handle_key(key(KeyCode::Tab), &mut state);

        assert_eq!(
            screen.explorer_cursor, 0,
            "a non-zero cursor past the end of the listing must be clamped back into range"
        );
    }

    #[test]
    fn explorer_render_cost_tracks_the_viewport_not_the_listing_length() {
        // The render formats only the rows the pane can show. Guarding that with an absolute
        // millisecond budget would just encode this machine's speed, so compare two renders of
        // the *same viewport* — one over a small directory, one over a 100x larger one. Slicing
        // makes them near-identical; formatting every row and letting `Paragraph::scroll` throw
        // the rest away (the shape this replaced) makes the large one scale with the listing.
        const SMALL: usize = 200;
        const WIDE: usize = 20_000;

        fn time_renders(rows: usize) -> std::time::Duration {
            let mut state = state_of(wide_directory_selection(rows));
            let mut screen = WhatScreen::new(state.selection());
            into_wide_leaf(&mut screen, &mut state);
            // Prime the row memo so this times the render alone, not the derivation.
            assert_eq!(screen.explorer_rows(&state).len(), rows);
            let started = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(render_to_lines(&screen, &state, 120, 30));
            }
            started.elapsed()
        }

        // Warm up first: the very first render in a test binary pays one-off allocator and
        // backend costs that would otherwise land entirely on whichever size runs first.
        time_renders(SMALL);
        let small = time_renders(SMALL);
        let wide = time_renders(WIDE);

        assert!(
            wide < small * 5,
            "rendering a {WIDE}-row directory took {wide:?} against {small:?} for a {SMALL}-row \
             one into the same viewport — render cost is scaling with the listing length again, \
             so the visible-window slice in `render_explorer` is gone"
        );
    }
}
