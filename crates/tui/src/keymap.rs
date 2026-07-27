//! The single keymap catalog: every shortcut's label **and** its canonical binding, in one
//! place (#74, spec #51/#61). Before this module the footer, the `?` overlay, and the
//! reserved-chord swallow guard each carried their own hand-maintained copy of the same chords —
//! so a label or a chord change was an N-place edit nothing gated, and the copies had already
//! drifted (`←→ choose` vs `←/→ choose`, `Enter confirm` vs `Enter activate`, …).
//!
//! The catalog is **two levels**, because a chord's *spelling* is context-free but its *verb* is
//! not — the same physical key means `into dir` in the panes, `commit` in a path field,
//! `activate` in a modal, `start backup` on REVIEW. So:
//!
//! - [`Chord`] owns only the context-free facts: how a key is spelled plus its canonical
//!   `(KeyCode, KeyModifiers)`. Defined once per key.
//! - [`Binding`] pairs a chord with *what it does here* ([`Chord`] + `verb` + overlay [`Group`]).
//!   Its label is `"{spelling} {verb}"`, interpolated — never a literal — so a label and the key
//!   it names cannot drift.
//!
//! The footer selects an ordered slice of bindings per context/[`ReflowTier`](crate::reflow);
//! the overlay renders every overlay-grouped binding under its group heading; the swallow guard
//! matches [`RESERVED`] via [`Chord::matches`]. Full central `KeyEvent → Action` routing stays
//! Phase 2 — this module is the source of truth for labels + canonical bindings and gives screens
//! a [`Chord::matches`] helper, but it does not own dispatch.
//!
//! Arrows and the `·` separator stay Unicode in both glyph sets (#61: only decorative glyphs
//! remap), so no glyph set threads into the catalog.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::reflow::ReflowTier;

/// The separator between footer/overlay labels — the catalog's one copy of it (#61: stays
/// Unicode in both glyph sets).
pub(crate) const SEP: &str = " · ";

/// A symbolic key id: how it is spelled plus its canonical `(KeyCode, KeyModifiers)`. Context-free
/// — the *verb* belongs to a [`Binding`], not here. The `spelling` is what the footer and overlay
/// print; for the standard chords it mirrors `code`/`mods` (a `CONTROL` chord is spelled `Ctrl-…`),
/// and the [consistency test](tests) enforces exactly that so a label can't name a different key
/// than it binds. A few entries are display-only composites (`↑↓`, `→/Enter`, `type to jump`)
/// whose `code`/`mods` carry a representative key; those are never matched against, only rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chord {
    pub spelling: &'static str,
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    /// Whether `event` is exactly this chord. Exact on both `code` and `mods` — a screen
    /// converting a literal `match KeyCode::Right + CONTROL` arm to the catalog wants precisely
    /// that chord, not a superset, and the reserved-swallow guard wants to swallow only the bare
    /// reserved chord.
    #[must_use]
    pub(crate) fn matches(&self, event: &KeyEvent) -> bool {
        self.code == event.code && self.mods == event.modifiers
    }
}

// --- Chords, defined once -------------------------------------------------------------------

/// The wizard step-motion chords. `pub(crate)` so `app`'s WHERE/WHAT screen-vs-wizard routing
/// matches them from the catalog via [`Chord::matches`] instead of restating `Left/Right + CONTROL`.
pub(crate) const CTRL_RIGHT: Chord = ctrl("Ctrl-→", KeyCode::Right);
pub(crate) const CTRL_LEFT: Chord = ctrl("Ctrl-←", KeyCode::Left);
const ENTER: Chord = plain("Enter", KeyCode::Enter);
const ESC: Chord = plain("Esc", KeyCode::Esc);
const F1: Chord = plain("F1", KeyCode::F(1));
const F2: Chord = plain("F2", KeyCode::F(2));
const F3: Chord = plain("F3", KeyCode::F(3));
/// The sole honored control chord (quit/abort). Exposed so `app` matches it from the catalog
/// rather than restating `Char('c') + CONTROL`.
pub(crate) const CTRL_C: Chord = ctrl("Ctrl-C", KeyCode::Char('c'));
const CTRL_A: Chord = ctrl("Ctrl-A", KeyCode::Char('a'));
const CTRL_D: Chord = ctrl("Ctrl-D", KeyCode::Char('d'));
const SPACE: Chord = plain("Space", KeyCode::Char(' '));
/// The WHAT step's Custom-paths verbs (#106/#107/#148) — the app's first **bare**-letter
/// bindings (every prior letter chord rides `Ctrl-`). Spelled lowercase (see `derived_spelling`'s
/// modifierless-`Char` case below) so the overlay/footer never suggest a Shift chord the reducer
/// doesn't bind.
const CHAR_A: Chord = plain("a", KeyCode::Char('a'));
const CHAR_R: Chord = plain("r", KeyCode::Char('r'));

// Display-only composites: `code`/`mods` are a representative key (never matched), the spelling
// carries the glyph set the reference prints.
const UP_DOWN: Chord = plain("↑↓", KeyCode::Up);
const UP_DOWN_SLASH: Chord = plain("↑/↓", KeyCode::Up);
const PGUP_PGDN: Chord = plain("PgUp/PgDn", KeyCode::PageUp);
const HOME_END: Chord = plain("Home/End", KeyCode::Home);
const TYPE_JUMP: Chord = plain("type to jump", KeyCode::Null);
const TYPE_PLAINTEXT: Chord = plain("type PLAINTEXT", KeyCode::Null);
const TAB_SHIFT_TAB: Chord = plain("Tab / Shift-Tab", KeyCode::Tab);
const RIGHT_ENTER: Chord = plain("→/Enter", KeyCode::Right);
const LEFT_BACKSPACE: Chord = plain("←/Backspace", KeyCode::Left);
const TAB_RIGHT_END: Chord = plain("Tab/→/End", KeyCode::Tab);
const LEFT_RIGHT: Chord = plain("←/→", KeyCode::Left);
const DIGITS: Chord = plain("1-4", KeyCode::Null);

const CTRL_Z: Chord = ctrl("Ctrl-Z", KeyCode::Char('z'));
const CTRL_S: Chord = ctrl("Ctrl-S", KeyCode::Char('s'));
const CTRL_Q: Chord = ctrl("Ctrl-Q", KeyCode::Char('q'));
const CTRL_BACKSLASH: Chord = ctrl("Ctrl-\\", KeyCode::Char('\\'));

const fn plain(spelling: &'static str, code: KeyCode) -> Chord {
    Chord {
        spelling,
        code,
        mods: KeyModifiers::NONE,
    }
}

const fn ctrl(spelling: &'static str, code: KeyCode) -> Chord {
    Chord {
        spelling,
        code,
        mods: KeyModifiers::CONTROL,
    }
}

/// The terminal-reserved chords the tool never binds (#51) — the single source shared by the
/// `app` swallow guard ([`is_reserved`]) and the overlay's `Reserved` group, so a reserved chord
/// is both swallowed and listed. `Ctrl-C` is *not* here: it is the one honored control chord.
pub(crate) const RESERVED: &[Chord] = &[CTRL_Z, CTRL_S, CTRL_Q, CTRL_BACKSLASH];

/// Whether `event` is a terminal-reserved chord the tool swallows everywhere. Drives the `app`
/// reducer's swallow guard so it no longer restates the reserved list as a `matches!` arm.
#[must_use]
pub(crate) fn is_reserved(event: &KeyEvent) -> bool {
    RESERVED.iter().any(|chord| chord.matches(event))
}

// --- Overlay grouping -----------------------------------------------------------------------

/// The `?` overlay's binding groups (#51 §4). The heading is kept distinct from a binding's label
/// so a group name (`REVIEW`, `Modals`, …) is never a second copy of the verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Move,
    Panes,
    PathField,
    Select,
    CustomPaths,
    Steps,
    Review,
    Reveal,
    Global,
    Modals,
    Run,
    Reserved,
}

impl Group {
    /// The bold left-column heading the overlay prints for this group.
    #[must_use]
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Panes => "Panes",
            Self::PathField => "Path field",
            Self::Select => "Select",
            Self::CustomPaths => "Custom paths",
            Self::Steps => "Steps",
            Self::Review => "REVIEW",
            Self::Reveal => "Reveal",
            Self::Global => "Global",
            Self::Modals => "Modals",
            Self::Run => "RUN",
            Self::Reserved => "Reserved",
        }
    }
}

/// The overlay's groups in display order (the prototype's order, #51 §4). `Reserved` is rendered
/// separately from [`RESERVED`] and so is not listed here.
pub(crate) const OVERLAY_GROUPS: &[Group] = &[
    Group::Move,
    Group::Panes,
    Group::PathField,
    Group::Select,
    Group::CustomPaths,
    Group::Steps,
    Group::Review,
    Group::Reveal,
    Group::Global,
    Group::Modals,
    Group::Run,
];

// --- Bindings -------------------------------------------------------------------------------

/// A chord paired with what it does in one context, plus the overlay group it lists under. The
/// atom the footer and overlay both render — `label()` is the single spelling+verb string, so the
/// two surfaces can never print a different label for the same key.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Binding {
    pub chord: Chord,
    pub verb: &'static str,
    /// The overlay group this binding lists under, or `None` for a footer-only binding the
    /// overlay does not carry (`Enter unlock`, the restore `↑/↓ scroll`, the reveal toggle).
    pub group: Option<Group>,
    /// An overlay-only qualifier appended after the label in the `?` panel (never in the footer):
    /// context the reference needs but a footer has no room for, e.g. `Enter close` is only live
    /// `(when done)`. Kept on the binding so the overlay renders it from one source rather than a
    /// parallel literal in `help.rs`.
    pub overlay_note: Option<&'static str>,
}

impl Binding {
    /// The rendered label: `"{spelling} {verb}"`, or just the spelling for a verbless composite
    /// row (`↑↓`, `type to jump`). Interpolated from the chord — never a literal.
    #[must_use]
    pub(crate) fn label(&self) -> String {
        if self.verb.is_empty() {
            self.chord.spelling.to_owned()
        } else {
            format!("{} {}", self.chord.spelling, self.verb)
        }
    }

    /// The label as the `?` overlay prints it: [`label`](Self::label) plus any [`overlay_note`]
    /// qualifier. The footer uses `label` and so never shows the note.
    #[must_use]
    pub(crate) fn overlay_label(&self) -> String {
        match self.overlay_note {
            Some(note) => format!("{} {note}", self.label()),
            None => self.label(),
        }
    }
}

const fn binding(chord: Chord, verb: &'static str, group: Group) -> Binding {
    Binding {
        chord,
        verb,
        group: Some(group),
        overlay_note: None,
    }
}

/// A footer-only binding the `?` overlay does not list.
const fn footer_only(chord: Chord, verb: &'static str) -> Binding {
    Binding {
        chord,
        verb,
        group: None,
        overlay_note: None,
    }
}

// Named bindings the footer selects. Each is also an entry in [`CATALOG`], which the drift-guard
// test enforces, so a footer can never emit a label that isn't in the catalog.
pub(crate) const NEXT: Binding = binding(CTRL_RIGHT, "next", Group::Steps);
pub(crate) const BACK_STEP: Binding = binding(CTRL_LEFT, "back", Group::Steps);
pub(crate) const JUMP: Binding = binding(DIGITS, "jump", Group::Steps);
pub(crate) const START_BACKUP: Binding = binding(ENTER, "start backup", Group::Review);
pub(crate) const HELP: Binding = binding(F1, "keys", Group::Global);
pub(crate) const ESC_BACK: Binding = binding(ESC, "back/close", Group::Global);
pub(crate) const QUIT: Binding = binding(CTRL_C, "quit/abort", Group::Global);
pub(crate) const CHOOSE: Binding = binding(LEFT_RIGHT, "choose", Group::Modals);
pub(crate) const CONFIRM: Binding = binding(ENTER, "confirm", Group::Modals);
pub(crate) const MODAL_CANCEL: Binding = binding(ESC, "cancel", Group::Modals);
pub(crate) const RUN_CANCEL: Binding = binding(CTRL_C, "cancel", Group::Run);
pub(crate) const CLOSE: Binding = Binding {
    chord: ENTER,
    verb: "close",
    group: Some(Group::Run),
    // `Enter close` is only live once the run is terminal; the footer omits this (a running RUN
    // shows no `Enter` hint), the overlay carries it.
    overlay_note: Some("(when done)"),
};
/// `a` adds a Custom path from anywhere in the WHAT step (#106/#107/#148) — the creative verb
/// reaches anywhere, unlike its destructive counterpart below.
pub(crate) const ADD_CUSTOM_PATH: Binding = binding(CHAR_A, "add", Group::CustomPaths);
/// `r` removes the focused Custom-paths row, pane-scoped (silently inert off the Custom-paths
/// Explorer rows) — deliberately narrower than `a`'s reach anywhere.
pub(crate) const REMOVE_CUSTOM_PATH: Binding = binding(CHAR_R, "remove", Group::CustomPaths);
pub(crate) const UNLOCK: Binding = footer_only(ENTER, "unlock");
pub(crate) const SCROLL: Binding = footer_only(UP_DOWN_SLASH, "scroll");
/// The restore unlock reveal toggle; the footer flips its verb to `hide` while revealed (a state
/// variant off one binding, not a parallel literal).
pub(crate) const REVEAL: Binding = footer_only(F2, "reveal");

/// Every binding, grouped in overlay order then the footer-only tail. The overlay renders each
/// entry whose `group` is `Some`; the footer selects named entries above. This is the one list
/// the drift-guard and overlay-coverage tests read.
pub(crate) const CATALOG: &[Binding] = &[
    // Move
    binding(UP_DOWN, "", Group::Move),
    binding(PGUP_PGDN, "", Group::Move),
    binding(HOME_END, "", Group::Move),
    binding(TYPE_JUMP, "", Group::Move),
    // Panes
    binding(TAB_SHIFT_TAB, "focus", Group::Panes),
    binding(RIGHT_ENTER, "into dir", Group::Panes),
    binding(LEFT_BACKSPACE, "up", Group::Panes),
    // Path field
    binding(TAB_RIGHT_END, "accept candidate", Group::PathField),
    binding(UP_DOWN, "pick sibling", Group::PathField),
    binding(ENTER, "commit", Group::PathField),
    // Select
    binding(SPACE, "toggle (dir = whole subtree)", Group::Select),
    binding(CTRL_A, "all", Group::Select),
    binding(CTRL_D, "none", Group::Select),
    // Custom paths
    ADD_CUSTOM_PATH,
    REMOVE_CUSTOM_PATH,
    // Steps
    NEXT,
    BACK_STEP,
    JUMP,
    // REVIEW
    START_BACKUP,
    // Reveal
    binding(F2, "passphrase", Group::Reveal),
    binding(F3, "hidden files", Group::Reveal),
    // Global
    HELP,
    ESC_BACK,
    QUIT,
    // Modals
    CHOOSE,
    CONFIRM,
    MODAL_CANCEL,
    binding(TYPE_PLAINTEXT, "", Group::Modals),
    // RUN
    RUN_CANCEL,
    CLOSE,
    // Footer-only (no overlay group)
    UNLOCK,
    SCROLL,
    REVEAL,
];

// --- Footer composition ---------------------------------------------------------------------
//
// Each context returns its already-joined footer line, built only from catalog labels (plus the
// two documented state variants: the incomplete-gate `(locked)` suffix and the reveal `hide`
// verb). Centralising it here is what lets the drift-guard test enumerate every footer.

fn join(labels: &[String]) -> String {
    labels.join(SEP)
}

/// The wizard footer for the earlier (non-REVIEW) steps, degrading down the reflow ladder but
/// always keeping `F1 keys` (#50). The forward chord dims with a `(locked)` suffix while the
/// current step is incomplete (#61).
#[must_use]
pub(crate) fn wizard_step_footer(forward_complete: bool, tier: ReflowTier) -> String {
    let forward = if forward_complete {
        NEXT.label()
    } else {
        format!("{} (locked)", NEXT.label())
    };
    let labels = match tier {
        ReflowTier::Comfortable | ReflowTier::Default => vec![
            forward,
            BACK_STEP.label(),
            JUMP.label(),
            ESC_BACK.label(),
            HELP.label(),
        ],
        ReflowTier::Tight => vec![forward, BACK_STEP.label(), HELP.label()],
        ReflowTier::Minimum | ReflowTier::Refuse => vec![forward, HELP.label()],
    };
    join(&labels)
}

/// The WHAT step's own footer (#106/#107/#148): the standard step footer plus `a add · r remove`,
/// so the app's first bare-letter chords are discoverable on first encounter rather than
/// overlay-only — the auto-open-on-empty behavior (#106) means a first-time user can land in the
/// add field having never seen `r` mentioned anywhere. `render_footer` selects this only while the
/// Custom-path field is closed (`!what.is_typing()`): while it's open, `a`/`r` are inert (the
/// field owns plain keys) and this footer would otherwise advertise two keys that do nothing.
#[must_use]
pub(crate) fn what_step_footer(forward_complete: bool, tier: ReflowTier) -> String {
    join(&[
        wizard_step_footer(forward_complete, tier),
        ADD_CUSTOM_PATH.label(),
        REMOVE_CUSTOM_PATH.label(),
    ])
}

/// The wizard footer on the terminal REVIEW step: the forward action is launching the run, so it
/// leads with `Enter start backup` rather than the step-motion chord (#61).
#[must_use]
pub(crate) fn wizard_review_footer(tier: ReflowTier) -> String {
    let labels = match tier {
        ReflowTier::Comfortable | ReflowTier::Default => vec![
            START_BACKUP.label(),
            BACK_STEP.label(),
            JUMP.label(),
            HELP.label(),
        ],
        ReflowTier::Tight => vec![START_BACKUP.label(), BACK_STEP.label(), HELP.label()],
        ReflowTier::Minimum | ReflowTier::Refuse => vec![START_BACKUP.label(), HELP.label()],
    };
    join(&labels)
}

/// The restore overwrite-confirm footer.
#[must_use]
pub(crate) fn restore_confirm_footer() -> String {
    join(&[
        CHOOSE.label(),
        CONFIRM.label(),
        MODAL_CANCEL.label(),
        HELP.label(),
    ])
}

/// The restore passphrase-unlock footer; the reveal hint flips to `F2 hide` while revealed.
#[must_use]
pub(crate) fn restore_unlock_footer(revealed: bool) -> String {
    let reveal = if revealed {
        format!("{} hide", REVEAL.chord.spelling)
    } else {
        REVEAL.label()
    };
    join(&[UNLOCK.label(), reveal, MODAL_CANCEL.label(), HELP.label()])
}

/// The footer for a running backup/restore worker: only `Ctrl-C cancel` matters here.
#[must_use]
pub(crate) fn running_footer() -> String {
    join(&[RUN_CANCEL.label(), HELP.label()])
}

/// The restore result footer; a partial report adds the scroll hint for its failure list.
#[must_use]
pub(crate) fn report_footer(partial: bool) -> String {
    if partial {
        join(&[SCROLL.label(), CLOSE.label(), HELP.label()])
    } else {
        join(&[CLOSE.label(), HELP.label()])
    }
}

/// The RUN screen footer once the run is terminal: `Enter close`.
#[must_use]
pub(crate) fn close_footer() -> String {
    join(&[CLOSE.label(), HELP.label()])
}

/// Just `F1 keys` — the footer for a transient/closing screen that carries only the overlay
/// pointer the footer always keeps (#50/#51).
#[must_use]
pub(crate) fn help_only_footer() -> String {
    HELP.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four normalizations #74 deliberately applies, so a reviewer can see them asserted:
    /// one canonical spelling/verb per row, replacing the drifted footer⟷overlay copies.
    #[test]
    fn normalizations_are_the_canonical_copies() {
        assert_eq!(CHOOSE.label(), "←/→ choose"); // was `←→ choose` in the footer
        assert_eq!(CONFIRM.label(), "Enter confirm"); // was `Enter activate` in the overlay
        assert_eq!(ESC_BACK.label(), "Esc back/close"); // was `Esc back` in the footer
        assert_eq!(START_BACKUP.label(), "Enter start backup"); // was `REVIEW: Enter start`
    }

    /// #106/#107/#148: the app's first bare-letter chords spell lowercase, not the uppercase
    /// every prior letter chord (always a `Ctrl-` chord) used — `A add`/`R remove` would
    /// misleadingly read as Shift chords the reducer never binds.
    #[test]
    fn bare_letter_custom_path_chords_spell_lowercase() {
        assert_eq!(ADD_CUSTOM_PATH.label(), "a add");
        assert_eq!(REMOVE_CUSTOM_PATH.label(), "r remove");
    }

    /// Drift guard: every label any footer can emit is a catalog binding label (or one of the two
    /// documented state variants). A footer built from a chord/verb not in the catalog fails here.
    #[test]
    fn every_footer_label_is_a_catalog_entry() {
        let mut allowed: Vec<String> = CATALOG.iter().map(Binding::label).collect();
        // The two state variants the footer composes off a base binding (#74 "binding + state
        // suffix", not a parallel literal).
        allowed.push(format!("{} (locked)", NEXT.label()));
        allowed.push(format!("{} hide", REVEAL.chord.spelling));

        let mut footers = Vec::new();
        for tier in [
            ReflowTier::Comfortable,
            ReflowTier::Default,
            ReflowTier::Tight,
            ReflowTier::Minimum,
            ReflowTier::Refuse,
        ] {
            footers.push(wizard_step_footer(true, tier));
            footers.push(wizard_step_footer(false, tier));
            footers.push(wizard_review_footer(tier));
            footers.push(what_step_footer(true, tier));
            footers.push(what_step_footer(false, tier));
        }
        footers.push(restore_confirm_footer());
        footers.push(restore_unlock_footer(false));
        footers.push(restore_unlock_footer(true));
        footers.push(running_footer());
        footers.push(report_footer(true));
        footers.push(report_footer(false));
        footers.push(close_footer());
        footers.push(help_only_footer());

        for footer in &footers {
            for label in footer.split(SEP) {
                assert!(
                    allowed.iter().any(|a| a == label),
                    "footer label {label:?} is not a catalog binding (footer: {footer:?})"
                );
            }
        }
    }

    /// Spelling ⟷ binding consistency: a chord whose mods include `CONTROL`/`ALT` has a spelling
    /// with the matching `Ctrl-`/`Alt-` prefix, and vice versa — guaranteed at the chord level, so
    /// a label and the key it names can't drift.
    #[test]
    fn chord_spelling_matches_its_modifiers() {
        let chords = CATALOG
            .iter()
            .map(|b| b.chord)
            .chain(RESERVED.iter().copied());
        for chord in chords {
            let ctrl = chord.mods.contains(KeyModifiers::CONTROL);
            let alt = chord.mods.contains(KeyModifiers::ALT);
            assert_eq!(
                ctrl,
                chord.spelling.starts_with("Ctrl-"),
                "{:?}: CONTROL and `Ctrl-` prefix must agree",
                chord.spelling
            );
            assert_eq!(
                alt,
                chord.spelling.starts_with("Alt-"),
                "{:?}: ALT and `Alt-` prefix must agree",
                chord.spelling
            );
        }
    }

    /// The spelling a standard (non-decorative) chord must have, derived from its `code`/`mods`,
    /// or `None` for the chords whose spelling is a custom glyph the catalog owns deliberately:
    /// the arrow chords (`→`/`←` are decorative per #61) and the display-only composites (whose
    /// `code` is only a representative key). This is the derivation the doc comment on [`Chord`]
    /// claims for standard chords; the test below enforces it so a spelling can't silently name a
    /// different key than it binds (`plain("Entr", KeyCode::Enter)` must fail).
    fn derived_spelling(chord: Chord) -> Option<String> {
        let ctrl = chord.mods.contains(KeyModifiers::CONTROL);
        let alt = chord.mods.contains(KeyModifiers::ALT);
        let base = match chord.code {
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Char(' ') => "Space".to_owned(),
            // A modified letter chord (today, only `Ctrl-A`/`Ctrl-D`) still uppercases — that's
            // the Shift-adjacent convention every prior letter chord in the catalog used, since
            // every one of them rode `Ctrl-`. A bare, modifierless letter (#106/#107/#148's `a`/
            // `r`, the app's first) spells lowercase instead: uppercasing it here would print
            // `A add`/`R remove`, advertising a Shift chord the reducer never binds.
            KeyCode::Char(c) if ctrl || alt => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            _ => return None,
        };
        let prefix = if ctrl {
            "Ctrl-"
        } else if alt {
            "Alt-"
        } else {
            ""
        };
        Some(format!("{prefix}{base}"))
    }

    /// Spelling ⟷ bound key consistency: a standard chord's spelling is exactly what
    /// [`derived_spelling`] computes from its `code`/`mods`, so the spelling names the key it
    /// binds (not just the right modifier prefix). Decorative-glyph and composite chords are
    /// exempt by construction (`derived_spelling` returns `None` for them).
    #[test]
    fn standard_chord_spelling_is_derivable_from_its_key() {
        let chords = CATALOG
            .iter()
            .map(|b| b.chord)
            .chain(RESERVED.iter().copied());
        for chord in chords {
            if let Some(expected) = derived_spelling(chord) {
                assert_eq!(
                    chord.spelling, expected,
                    "spelling must name the key it binds ({:?})",
                    chord.code
                );
            }
        }
    }

    /// Reserved single-source: [`RESERVED`] drives both the swallow guard and (via the overlay,
    /// tested in `help`) the `Reserved` group. Here: exactly the reserved chords are swallowed,
    /// and the honored `Ctrl-C` is not.
    #[test]
    fn reserved_set_drives_the_swallow_guard() {
        use crossterm::event::KeyEventKind;
        for chord in RESERVED {
            let event = KeyEvent::new_with_kind(chord.code, chord.mods, KeyEventKind::Press);
            assert!(
                is_reserved(&event),
                "{:?} must be swallowed",
                chord.spelling
            );
        }
        let ctrl_c = KeyEvent::new_with_kind(CTRL_C.code, CTRL_C.mods, KeyEventKind::Press);
        assert!(!is_reserved(&ctrl_c), "Ctrl-C is honored, not reserved");
    }
}
