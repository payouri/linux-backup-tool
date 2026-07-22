//! The wizard's WHERE step body (#70): an Output-location pick-list plus a custom-path field
//! with fish-style autocomplete (live candidate list + inline ghost-text).
//!
//! Per ADR-0005 D6 the `tui` performs **no** filesystem *content* I/O of its own: every
//! directory listing goes through [`core::fs_probe::list_dir`](linux_backup_core::fs_probe::list_dir),
//! and the writable/existence checks go through [`core::location`](linux_backup_core::location).
//! The one carve-out is env-var/`~` expansion of a user-typed path: env vars are ambient run
//! facts, so the literal `~`/`$VAR` stays in the buffer and expansion happens here at the `tui`
//! boundary through the injected `getenv` resolver (ADR-0004), never inside `core`.
//!
//! ## Controls (#52 / #51 keymap)
//!
//! The step has two focus modes. In `Picking` the pick-list rows drive `↑`/`↓`, and `Enter`/`→`
//! either binds a location row or opens the custom-path field (its trailing row). In `Typing`
//! the field is focused: characters edit the buffer, `Tab` completes the longest common prefix
//! (or descends into an unambiguous single directory, one path segment at a time, stopping at
//! each directory's trailing `/`), `→`/`End` accept the whole ghost suggestion, `↑`/`↓` move the
//! candidate cursor, `Enter` accepts the highlighted candidate or — once the buffer already
//! names it — commits the location, and `Esc` closes the field back to the pick-list.
//!
//! Completion is over **existing paths only** (the tool never creates a directory, #52). Dotfiles
//! stay hidden until the current segment starts with `.`; a reveal-all toggle is exposed as
//! [`WhereStep::toggle_show_hidden`] for #73 to wire to `F3`. Non-root scope is proactive and
//! lexical (matching `catalog::validate_custom_path`): an out-of-scope path offers no candidates
//! and shows `⚠ outside ~`.

use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use linux_backup_core::location::{self, FreeSpaceWarning, LocationRow};
use linux_backup_core::{catalog, Privilege, RunContext};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::glyphs::Glyphs;
use crate::path_autocomplete::{self, Autocomplete, Getenv};

/// Which region of the WHERE body has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Navigating the location pick-list (its last row opens the custom-path field).
    Picking,
    /// Editing the custom-path field, with autocomplete live.
    Typing,
}

/// A directly-bindable pick-list location: `$HOME` or an already-mounted drive. `available_space`
/// is `Some` only for a mounted drive (whose `DiskInfo` carries it) — it drives the advisory
/// free-space warning and is absent for `$HOME`, whose free space is unknown here.
#[derive(Debug, Clone)]
struct LocationCandidate {
    label: String,
    path: PathBuf,
    available_space: Option<u64>,
}

/// The currently-committed Output location and the result of its parent-writable probe (#70's
/// WHERE gate). `writable` is the injected-`core` `ensure_writable` result; the advisory
/// free-space `available_space` is carried so the warning survives leaving `Picking`.
#[derive(Debug, Clone)]
struct Chosen {
    location: PathBuf,
    writable: bool,
    available_space: Option<u64>,
}

/// The WHERE step's view state, held by [`crate::app::App`] around the wizard's `SelectionModel`
/// (ADR-0005: view state lives in `App`, never inside the model).
#[derive(Debug, Clone)]
pub struct WhereStep {
    locations: Vec<LocationCandidate>,
    /// Row cursor in `Picking`; `locations.len()` is the virtual "Type a path…" row.
    cursor: usize,
    mode: Mode,
    /// The literal custom-path buffer — `~`/`$VAR` preserved, expansion internal (ADR-0004).
    buffer: String,
    /// The highlighted autocomplete candidate; clamped to the live candidate count on move.
    ac_selected: usize,
    /// The reveal-all-dotfiles toggle (#52); the `F3` binding that flips it is #73's.
    show_hidden: bool,
    chosen: Option<Chosen>,
    home: PathBuf,
    privilege: Privilege,
}

impl WhereStep {
    /// Builds the step for a real run: discovers bindable locations via `core::location` and
    /// pre-selects (and validates) the context's Output location, so the step opens on a
    /// populated, gate-checked pick-list.
    #[must_use]
    pub fn from_context(ctx: &RunContext) -> Self {
        let disks = location::discover_disks();
        let unmounted = location::run_lsblk().unwrap_or_default();
        let pick = location::build_pick_list(&disks, &unmounted, &ctx.home, &ctx.user);
        let locations = pick.rows.iter().filter_map(bindable_candidate).collect();

        let mut step = Self {
            locations,
            cursor: 0,
            mode: Mode::Picking,
            buffer: String::new(),
            ac_selected: 0,
            show_hidden: false,
            chosen: None,
            home: ctx.home.clone(),
            privilege: ctx.privilege,
        };
        step.preselect(&ctx.output_location);
        step
    }

    /// An inert step for the non-wizard screens that never show a WHERE body (the placeholder
    /// routes). It probes no hardware — those screens are separate invocations of the tool.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            locations: Vec::new(),
            cursor: 0,
            mode: Mode::Picking,
            buffer: String::new(),
            ac_selected: 0,
            show_hidden: false,
            chosen: None,
            home: PathBuf::new(),
            privilege: Privilege::NonRoot,
        }
    }

    /// The chosen Output location and its writable-probe result, for the wizard's WHERE gate.
    /// `(None, false)` when nothing is bound yet.
    #[must_use]
    pub fn chosen(&self) -> (Option<PathBuf>, bool) {
        match &self.chosen {
            Some(c) => (Some(c.location.clone()), c.writable),
            None => (None, false),
        }
    }

    /// Whether the custom-path field is focused — the signal `App` uses to suppress the `1`–`4`
    /// step jump so typed digits reach the field (#51).
    #[must_use]
    pub fn is_typing(&self) -> bool {
        matches!(self.mode, Mode::Typing)
    }

    /// Flips the reveal-all-dotfiles toggle, wired to the `F3` reveal binding (#51/#73). The
    /// key is claimed globally in [`crate::app`] so it stays live while the custom-path field
    /// owns the plain keys.
    pub fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
    }

    /// Whether dotfiles are currently revealed in the autocomplete — the observable the app's
    /// `F3` wiring test asserts against.
    #[cfg(test)]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Folds one key press into the step. `App` routes only the body keys here — the global
    /// step-motion chords stay with the wizard (#51).
    pub fn handle_key(&mut self, key: KeyEvent, getenv: Getenv) {
        match self.mode {
            Mode::Picking => self.handle_picking(key),
            Mode::Typing => self.handle_typing(key, getenv),
        }
    }

    fn handle_picking(&mut self, key: KeyEvent) {
        let custom_row = self.locations.len();
        match key.code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(custom_row),
            KeyCode::Enter | KeyCode::Right => {
                if self.cursor == custom_row {
                    self.mode = Mode::Typing;
                } else {
                    self.bind_row(self.cursor);
                }
            }
            _ => {}
        }
    }

    fn handle_typing(&mut self, key: KeyEvent, getenv: Getenv) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Picking,
            KeyCode::Tab => self.complete_tab(getenv),
            KeyCode::Right | KeyCode::End => self.accept_ghost(getenv),
            KeyCode::Up => self.ac_selected = self.ac_selected.saturating_sub(1),
            KeyCode::Down => {
                let view = self.autocomplete(getenv);
                if !view.candidates.is_empty() {
                    self.ac_selected = (self.ac_selected + 1).min(view.candidates.len() - 1);
                }
            }
            KeyCode::Enter => self.accept_or_commit(getenv),
            KeyCode::Backspace => {
                self.buffer.pop();
                self.ac_selected = 0;
            }
            KeyCode::Char(c) => {
                self.buffer.push(c);
                self.ac_selected = 0;
            }
            _ => {}
        }
    }

    /// Binds `idx`'s location as the Output location, probing writability through `core` (the
    /// WHERE gate's parent-writable check, #70).
    fn bind_row(&mut self, idx: usize) {
        if let Some(loc) = self.locations.get(idx) {
            self.chosen = Some(Chosen {
                location: loc.path.clone(),
                writable: location::ensure_writable(&loc.path).is_ok(),
                available_space: loc.available_space,
            });
        }
    }

    /// Pre-selects and validates the seeded Output location, parking the cursor on its row when
    /// one matches (else leaving the pick-list on its first row).
    fn preselect(&mut self, location: &Path) {
        if let Some(idx) = self.locations.iter().position(|l| l.path == location) {
            self.cursor = idx;
            self.bind_row(idx);
        }
    }

    /// `Tab`: complete the longest common prefix, or descend into an unambiguous single
    /// directory — component-by-component, stopping at each directory's trailing `/` (#52).
    fn complete_tab(&mut self, getenv: Getenv) {
        let view = self.autocomplete(getenv);
        path_autocomplete::complete_tab(&mut self.buffer, &view);
        self.ac_selected = 0;
    }

    /// `→`/`End`: accept the whole ghost — the highlighted candidate, eagerly (fish-style), with
    /// a trailing `/` when it's a directory so the next segment can follow.
    fn accept_ghost(&mut self, getenv: Getenv) {
        let view = self.autocomplete(getenv);
        path_autocomplete::accept_ghost(&mut self.buffer, &view);
        self.ac_selected = 0;
    }

    /// `Enter`: accept the highlighted candidate when it would extend the buffer, otherwise
    /// commit the buffer as the Output location. So a partial fragment descends/extends (the
    /// candidate list drives `Enter`), while a buffer that already names its target confirms it.
    fn accept_or_commit(&mut self, getenv: Getenv) {
        let view = self.autocomplete(getenv);
        let (_, fragment) = path_autocomplete::split_buffer(&self.buffer);
        match view.candidates.get(view.selected) {
            Some(candidate) if candidate.name.to_string_lossy() != fragment => {
                self.accept_ghost(getenv);
            }
            _ => self.commit(getenv),
        }
    }

    /// Commits the buffer as the Output location: expand at the `tui` boundary, refuse an
    /// out-of-scope path silently (the inline `⚠ outside ~` already explains it), then record
    /// the location and its writable probe for the gate.
    fn commit(&mut self, getenv: Getenv) {
        let expanded = location::expand_path(&self.buffer, &self.home, getenv);
        if catalog::validate_custom_path(&expanded, &self.home, self.privilege).is_err() {
            return;
        }
        self.chosen = Some(Chosen {
            writable: location::ensure_writable(&expanded).is_ok(),
            location: expanded,
            available_space: None,
        });
    }

    /// Computes the live candidate listing for the current buffer via the shared engine
    /// ([`crate::path_autocomplete`]) — scope, dotfile hiding, prefix narrowing, and dirs-first
    /// ordering over `core`'s directory listing (ADR-0005 D6).
    fn autocomplete(&self, getenv: Getenv) -> Autocomplete {
        path_autocomplete::compute(
            &self.buffer,
            self.ac_selected,
            self.show_hidden,
            &self.home,
            self.privilege,
            getenv,
        )
    }

    /// The ghost-text tail for the highlighted candidate — delegated to the shared engine.
    fn ghost(&self, view: &Autocomplete) -> String {
        path_autocomplete::ghost(&self.buffer, view)
    }

    /// The advisory free-space warning for the location currently in focus (the cursor row while
    /// picking, else the bound location). Advisory only — it never blocks the WHERE gate (#61).
    fn active_warning(&self, selection_size: u64) -> Option<(FreeSpaceWarning, String)> {
        let (available, label) = match self.mode {
            Mode::Picking => {
                let loc = self.locations.get(self.cursor)?;
                (loc.available_space?, loc.label.clone())
            }
            Mode::Typing => {
                let chosen = self.chosen.as_ref()?;
                (
                    chosen.available_space?,
                    chosen.location.display().to_string(),
                )
            }
        };
        location::free_space_warning(selection_size, available).map(|w| (w, label))
    }

    /// Draws the WHERE body: the location pick-list, the custom-path field with its live
    /// candidate list and ghost-text, the advisory space warning, and the standing scope hint.
    /// Lines past `area.height` are dropped so the body never overflows the reflow frame.
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        selection_size: u64,
        getenv: Getenv,
        glyphs: Glyphs,
    ) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let warn = Style::default().fg(Color::Yellow);
        let ok = Style::default().fg(Color::Green);

        let mut lines: Vec<Line> = vec![Line::from("WHERE to write the backup")];

        for (idx, loc) in self.locations.iter().enumerate() {
            let picked = self.mode == Mode::Picking && self.cursor == idx;
            let marker = if picked {
                format!("({})", glyphs.bullet)
            } else {
                "( )".to_owned()
            };
            let mut spans = vec![Span::raw(format!("{marker} {}", loc.label))];
            let tight = loc
                .available_space
                .and_then(|a| location::free_space_warning(selection_size, a))
                .is_some();
            if tight {
                spans.push(Span::styled(format!("  {} tight", glyphs.warn), warn));
            }
            lines.push(Line::from(spans));
        }

        let custom_row = self.locations.len();
        let custom_picked =
            self.mode == Mode::Typing || (self.mode == Mode::Picking && self.cursor == custom_row);
        let custom_marker = if custom_picked {
            format!("({})", glyphs.bullet)
        } else {
            "( )".to_owned()
        };

        if self.mode == Mode::Typing {
            let view = self.autocomplete(getenv);
            let ghost = self.ghost(&view);
            let mut field = vec![
                Span::raw(format!("{custom_marker} Type a path…  [ ")),
                Span::raw(self.buffer.clone()),
            ];
            if !ghost.is_empty() {
                field.push(Span::styled(ghost, dim));
            }
            field.push(Span::raw(" ]"));
            if !view.out_of_scope && !self.buffer.is_empty() {
                let expanded = location::expand_path(&self.buffer, &self.home, getenv);
                if location::ensure_writable(&expanded).is_ok() {
                    field.push(Span::styled(format!("  {} writable", glyphs.check), ok));
                }
            }
            lines.push(Line::from(field));

            lines.extend(path_autocomplete::candidate_lines(&view, glyphs));
        } else {
            lines.push(Line::from(format!("{custom_marker} Type a path…")));
        }

        if let Some((warning, label)) = self.active_warning(selection_size) {
            lines.push(Line::styled(
                format!(
                    "{} {} selection is close to {}'s {} free space.",
                    glyphs.warn,
                    format_size(warning.selection_size),
                    label,
                    format_size(warning.available_space)
                ),
                warn,
            ));
        }

        lines.push(Line::styled(
            path_autocomplete::scope_hint(self.privilege).to_string(),
            dim,
        ));

        let visible: Vec<Line> = lines.into_iter().take(area.height as usize).collect();
        frame.render_widget(Paragraph::new(visible), area);
    }
}

/// Maps a bindable pick-list row (`$HOME` or a mounted drive) to a candidate, dropping the
/// unmounted-USB / rescan / custom rows — mounting and the custom field are handled elsewhere.
fn bindable_candidate(row: &LocationRow) -> Option<LocationCandidate> {
    match row {
        LocationRow::Home { path } => Some(LocationCandidate {
            label: format!("$HOME  ({})", path.display()),
            path: path.clone(),
            available_space: None,
        }),
        LocationRow::Mount {
            mount_point,
            file_system,
            available_space,
            ..
        } => Some(LocationCandidate {
            label: format!(
                "{}  [{}]  {} free",
                mount_point.display(),
                file_system,
                format_size(*available_space)
            ),
            path: mount_point.clone(),
            available_space: Some(*available_space),
        }),
        LocationRow::UnmountedUsbPartition { .. }
        | LocationRow::RescanDrives
        | LocationRow::CustomPath => None,
    }
}

/// Human-readable byte count (`B`/`KB`/`MB`/`GB`, 1024-based). Kept local to this screen module
/// rather than coupling the screens through a shared helper.
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
    use crossterm::event::{KeyEventKind, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    /// A step rooted at `home`, non-root, with no pick-list rows and the custom field already
    /// focused — the isolated harness the autocomplete reducer tests drive.
    fn typing_step(home: &Path) -> WhereStep {
        WhereStep {
            locations: Vec::new(),
            cursor: 0,
            mode: Mode::Typing,
            buffer: String::new(),
            ac_selected: 0,
            show_hidden: false,
            chosen: None,
            home: home.to_path_buf(),
            privilege: Privilege::NonRoot,
        }
    }

    fn render_lines(step: &WhereStep, selection_size: u64, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend terminal");
        terminal
            .draw(|frame| {
                step.render(
                    frame,
                    frame.area(),
                    selection_size,
                    &no_env,
                    Glyphs::UNICODE,
                );
            })
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
    fn tab_completes_the_longest_common_prefix_when_ambiguous() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for dir in ["Documents", "Downloads", "Docker"] {
            fs::create_dir(tmp.path().join(dir)).expect("create dir");
        }
        let mut step = typing_step(tmp.path());
        step.buffer = "~/D".to_string();
        step.complete_tab(&no_env);
        // Extends to the common prefix "Do", stopping short of any single leaf (still ambiguous).
        assert_eq!(step.buffer, "~/Do");
    }

    #[test]
    fn tab_descends_into_an_unambiguous_single_directory_and_stops_at_the_slash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("projects")).expect("create dir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/pro".to_string();
        step.complete_tab(&no_env);
        // Component-by-component: the single dir is completed with its trailing `/` and Tab stops.
        assert_eq!(step.buffer, "~/projects/");
    }

    #[test]
    fn right_accepts_the_whole_ghost_suggestion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("Documents")).expect("create dir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/Doc".to_string();
        step.handle_key(key(KeyCode::Right), &no_env);
        assert_eq!(step.buffer, "~/Documents/");
    }

    #[test]
    fn down_and_up_move_the_candidate_cursor_and_reflect_in_the_ghost() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for dir in ["alpha", "beta", "gamma"] {
            fs::create_dir(tmp.path().join(dir)).expect("create dir");
        }
        let mut step = typing_step(tmp.path());
        step.buffer = "~/".to_string();
        // Cursor starts on the first candidate (alpha); its ghost is the whole name + slash.
        assert_eq!(step.ghost(&step.autocomplete(&no_env)), "alpha/");
        step.handle_key(key(KeyCode::Down), &no_env);
        assert_eq!(step.ghost(&step.autocomplete(&no_env)), "beta/");
        step.handle_key(key(KeyCode::Up), &no_env);
        assert_eq!(step.ghost(&step.autocomplete(&no_env)), "alpha/");
    }

    #[test]
    fn dotfiles_are_hidden_until_the_segment_starts_with_a_dot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".config")).expect("create dotdir");
        fs::create_dir(tmp.path().join("visible")).expect("create dir");
        let mut step = typing_step(tmp.path());

        step.buffer = "~/".to_string();
        let names: Vec<String> = step
            .autocomplete(&no_env)
            .candidates
            .iter()
            .map(|c| c.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["visible".to_string()]);

        step.buffer = "~/.".to_string();
        let names: Vec<String> = step
            .autocomplete(&no_env)
            .candidates
            .iter()
            .map(|c| c.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![".config".to_string()]);
    }

    #[test]
    fn show_hidden_toggle_reveals_dotfiles_without_a_leading_dot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".config")).expect("create dotdir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/".to_string();
        assert!(step.autocomplete(&no_env).candidates.is_empty());
        step.toggle_show_hidden();
        assert_eq!(step.autocomplete(&no_env).candidates.len(), 1);
    }

    #[test]
    fn non_root_out_of_scope_path_offers_no_candidates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.buffer = "/etc".to_string();
        let view = step.autocomplete(&no_env);
        assert!(view.out_of_scope);
        assert!(view.candidates.is_empty());
    }

    #[test]
    fn empty_buffer_is_not_out_of_scope_for_non_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let step = typing_step(tmp.path());
        // Opening the field (empty buffer) must not flag `⚠ outside ~` before anything is typed.
        let view = step.autocomplete(&no_env);
        assert!(!view.out_of_scope);
        assert!(view.candidates.is_empty());
    }

    #[test]
    fn root_ranges_outside_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.privilege = Privilege::Root;
        step.buffer = "/".to_string();
        // Root is never out of scope; `/` lists (at least some) real root entries.
        assert!(!step.autocomplete(&no_env).out_of_scope);
    }

    #[test]
    fn candidates_are_dirs_first_then_files_alphabetical() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("aaa.txt"), b"x").expect("write file");
        fs::create_dir(tmp.path().join("zdir")).expect("create dir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/".to_string();
        let names: Vec<String> = step
            .autocomplete(&no_env)
            .candidates
            .iter()
            .map(|c| c.name.to_string_lossy().into_owned())
            .collect();
        // The directory sorts before the file even though it's alphabetically later.
        assert_eq!(names, vec!["zdir".to_string(), "aaa.txt".to_string()]);
    }

    #[test]
    fn enter_commits_an_existing_writable_directory_completing_the_gate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("backups")).expect("create dir");
        let mut step = typing_step(tmp.path());
        // Buffer already names the target exactly (trailing slash) — Enter commits it.
        step.buffer = "~/backups/".to_string();
        step.handle_key(key(KeyCode::Enter), &no_env);
        let (location, writable) = step.chosen();
        assert_eq!(location, Some(tmp.path().join("backups")));
        assert!(
            writable,
            "an existing directory under home must probe writable"
        );
    }

    #[test]
    fn enter_on_a_partial_fragment_accepts_the_highlighted_candidate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("backups")).expect("create dir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/back".to_string();
        step.handle_key(key(KeyCode::Enter), &no_env);
        // Enter drove the candidate list (extended/descended) rather than committing.
        assert_eq!(step.buffer, "~/backups/");
        assert_eq!(step.chosen(), (None, false));
    }

    #[test]
    fn expands_env_vars_through_the_injected_resolver() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("target")).expect("create dir");
        let base = tmp.path().to_path_buf();
        let getenv =
            move |name: &str| (name == "BASE").then(|| base.to_string_lossy().into_owned());
        let mut step = typing_step(tmp.path());
        // The literal `$BASE` stays in the buffer; expansion is internal at the boundary.
        step.buffer = "$BASE/tar".to_string();
        step.complete_tab(&getenv);
        assert_eq!(step.buffer, "$BASE/target/");
    }

    #[test]
    fn binding_a_location_row_probes_writability_for_the_gate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.mode = Mode::Picking;
        step.locations = vec![LocationCandidate {
            label: "$HOME".to_string(),
            path: tmp.path().to_path_buf(),
            available_space: None,
        }];
        step.cursor = 0;
        step.handle_key(key(KeyCode::Enter), &no_env);
        assert_eq!(step.chosen(), (Some(tmp.path().to_path_buf()), true));
    }

    #[test]
    fn advisory_tight_space_never_blocks_the_gate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.mode = Mode::Picking;
        // A tiny drive that the selection dwarfs — the free-space warning fires.
        step.locations = vec![LocationCandidate {
            label: "USB  [vfat]  100 B free".to_string(),
            path: tmp.path().to_path_buf(),
            available_space: Some(100),
        }];
        step.cursor = 0;
        step.handle_key(key(KeyCode::Enter), &no_env);
        // The location is still bound and writable — the advisory warning did not block it.
        assert_eq!(step.chosen(), (Some(tmp.path().to_path_buf()), true));
        assert!(step.active_warning(10_000).is_some());
    }

    #[test]
    fn render_shows_the_candidate_list_ghost_and_scope_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("Documents")).expect("create dir");
        let mut step = typing_step(tmp.path());
        step.buffer = "~/Doc".to_string();
        let lines = render_lines(&step, 0, 70, 14);
        let joined = lines.join("\n");
        // The candidate list shows the match; the field's ghost extends "~/Doc" to "~/Documents/".
        assert!(joined.contains("Documents"), "candidate list: {joined}");
        assert!(joined.contains("~/Documents/"), "ghost text: {joined}");
        assert!(joined.contains("under ~"), "scope hint: {joined}");
    }

    #[test]
    fn render_shows_writable_status_for_a_committed_directory_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        // A buffer naming an existing directory under home shows the writable affordance.
        step.buffer = "~/".to_string();
        let lines = render_lines(&step, 0, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("✓ writable")),
            "expected the writable status, got: {lines:?}"
        );
    }

    #[test]
    fn render_shows_out_of_scope_warning_for_non_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.buffer = "/etc".to_string();
        let lines = render_lines(&step, 0, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("outside ~")),
            "expected the out-of-scope warning, got: {lines:?}"
        );
    }

    #[test]
    fn render_shows_more_footer_when_candidates_overflow_the_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..path_autocomplete::CANDIDATE_CAP + 5 {
            fs::create_dir(tmp.path().join(format!("dir{i:02}"))).expect("create dir");
        }
        let mut step = typing_step(tmp.path());
        step.buffer = "~/".to_string();
        let lines = render_lines(&step, 0, 70, 20);
        assert!(
            lines.iter().any(|l| l.contains("more — keep typing")),
            "expected the +M more footer, got: {lines:?}"
        );
    }

    #[test]
    fn render_marks_a_tight_location_row() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut step = typing_step(tmp.path());
        step.mode = Mode::Picking;
        step.locations = vec![LocationCandidate {
            label: "USB  [vfat]  100 B free".to_string(),
            path: tmp.path().to_path_buf(),
            available_space: Some(100),
        }];
        step.cursor = 0;
        let lines = render_lines(&step, 10_000, 70, 14);
        assert!(
            lines.iter().any(|l| l.contains("⚠ tight")),
            "expected the tight tag, got: {lines:?}"
        );
    }
}
