//! The encryption wizard step's body (build #69): the required-vs-offered passphrase surface,
//! the `F2` reveal, the live match indicator, the invalidation banner, and the plaintext
//! second-confirmation modal.
//!
//! Per ADR-0007 the encryption step is **always present**. What sensitivity changes is the
//! *behaviour*, not the existence of the step:
//!
//! - **Offered** (nothing Sensitive): the step is complete on arrival (default plaintext), yet
//!   still renders an opt-in passphrase path — a non-Sensitive backup can be encrypted anyway.
//! - **Required** (Sensitive Selection): whole-archive encryption is forced with the triggering
//!   Categories named; the only escape is the [`PlaintextModal`] second confirmation, which
//!   names the Sensitive content and demands the literal word `PLAINTEXT` before it will honour
//!   disabling encryption.
//!
//! This module owns only the encryption step's **view state** (ADR-0005: input buffers and
//! focus live in `tui` around the model, never inside it) plus its own field-editing reducer
//! and render. The wizard-level decisions — committing the typed passphrase (the safe harbor),
//! recording a plaintext decision, and the forward gate — stay in [`crate::wizard`], which
//! drives this screen through the pub(crate) surface below.
//!
//! **Secrecy invariant.** Both passphrase buffers are `secrecy::SecretString`, never a plain
//! `String`. Editing rebuilds the `SecretString` so the value only ever lives zeroized; reveal
//! ([`render`] with `reveal == true`) formats `expose_secret()` into a `Span` for that one
//! frame and retains nothing. The plaintext confirmation word is an ordinary `String` — it is
//! not a secret.

use crossterm::event::{KeyCode, KeyEvent};
use linux_backup_core::crypto;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use secrecy::{ExposeSecret, SecretString};

use crate::glyphs::Glyphs;
use crate::wizard::Wizard;

/// The exact word the plaintext modal demands before `[ Confirm ]` enables (#61 copy sheet).
const PLAINTEXT_WORD: &str = "PLAINTEXT";

/// The invalidation banner shown in the body while a completed plaintext decision has been
/// overtaken by newly-added Sensitive content (#61, verbatim). It steers to the safe action;
/// the plaintext escape stays the un-advertised `disable encryption (plaintext)` button below.
/// The leading `⚠` is prepended at render from the active glyph set (#73), so this holds the
/// message text alone.
const INVALIDATION_BANNER: &str = "Sensitive content added — set a passphrase.";

/// A focusable control on the encryption body. The valid set depends on the mode (see
/// [`EncryptionScreen::fields`]): the offered radio only exists when nothing is Sensitive, the
/// disable button only when it is, and the passphrase fields only when a passphrase is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Offered mode's plaintext-vs-encrypt radio (a single focus; `←`/`→`/`Space` choose).
    Radio,
    /// The passphrase entry field.
    Passphrase,
    /// The confirm field the match indicator compares against.
    Confirm,
    /// Required mode's `disable encryption (plaintext)` button, which opens the modal.
    Disable,
}

/// The live match-indicator state for the confirm field (#61 copy sheet). The indicator is
/// silent until *both* fields have content, so a half-typed passphrase never flashes a
/// premature mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchState {
    /// One or both fields are empty — nothing is shown.
    Silent,
    /// Both fields have content and differ.
    Mismatch,
    /// Both fields have content and are equal.
    Match,
}

/// The outcome of validating the two buffers when a forward move is attempted — the wizard
/// turns this into a committed passphrase, a gate banner, or (empty + already decided) a plain
/// advance.
#[derive(Debug)]
pub(crate) enum Commit {
    /// A non-empty passphrase that matches its confirm field: commit it as the safe harbor.
    Passphrase(SecretString),
    /// The passphrase field is empty.
    Empty,
    /// Both fields have content but differ.
    Mismatch,
}

/// What the wizard must do after the modal handled a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalOutcome {
    /// The modal is still open and captive; nothing changes at the wizard level.
    Stay,
    /// The modal closed (cancelled) without a decision.
    Closed,
    /// The user confirmed writing plaintext — the wizard records the plaintext decision.
    ConfirmPlaintext,
}

/// The plaintext second-confirmation modal (#61): naming the Sensitive Categories, it disables
/// encryption on a Sensitive Selection only after the literal word `PLAINTEXT` is typed. The
/// named Categories are derived at render time from the Selection, so the modal stores only the
/// transient confirmation text and which button is focused.
#[derive(Debug, Clone)]
struct PlaintextModal {
    /// The confirmation word the user is typing; `[ Confirm ]` enables only at an exact match.
    typed: String,
    /// The focused button. `Cancel` is focused by default (this is a destructive escape).
    focus: ModalButton,
}

/// The modal's two buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalButton {
    Cancel,
    Confirm,
}

impl PlaintextModal {
    /// Opens the modal with `[ Cancel ]` focused (the safe default) and an empty entry.
    fn new() -> Self {
        Self {
            typed: String::new(),
            focus: ModalButton::Cancel,
        }
    }

    /// Whether `[ Confirm ]` is enabled: only when the typed text equals `PLAINTEXT` exactly.
    fn confirm_enabled(&self) -> bool {
        self.typed == PLAINTEXT_WORD
    }

    /// Moves focus between the two buttons.
    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ModalButton::Cancel => ModalButton::Confirm,
            ModalButton::Confirm => ModalButton::Cancel,
        };
    }
}

/// The encryption step's view state: the two passphrase buffers, which control has focus,
/// whether the offered opt-in has chosen encryption, and the modal when open.
#[derive(Debug, Clone)]
pub(crate) struct EncryptionScreen {
    passphrase: SecretString,
    confirm: SecretString,
    focus: Field,
    /// Offered mode only: whether the `Encrypt with a passphrase` radio is chosen. Meaningless
    /// (and ignored) in required mode, where a passphrase is always wanted.
    encrypt_opt_in: bool,
    modal: Option<PlaintextModal>,
}

impl Default for EncryptionScreen {
    fn default() -> Self {
        Self {
            passphrase: SecretString::from(String::new()),
            confirm: SecretString::from(String::new()),
            // Passphrase is the first field in both modes' field lists except pure
            // leave-plaintext offered mode, where `normalize_focus` snaps it to the radio.
            focus: Field::Passphrase,
            encrypt_opt_in: false,
            modal: None,
        }
    }
}

impl EncryptionScreen {
    /// Whether the plaintext modal is currently open and capturing input.
    pub(crate) fn is_modal_open(&self) -> bool {
        self.modal.is_some()
    }

    /// The ordered focusable controls for the current mode. Required mode always wants a
    /// passphrase (plus the disable escape); offered mode shows the radio, and the passphrase
    /// fields only once encryption has been opted into.
    fn fields(&self, sensitive: bool) -> Vec<Field> {
        if sensitive {
            vec![Field::Passphrase, Field::Confirm, Field::Disable]
        } else if self.encrypt_opt_in {
            vec![Field::Radio, Field::Passphrase, Field::Confirm]
        } else {
            vec![Field::Radio]
        }
    }

    /// Snaps focus back onto a valid control after a mode/opt-in change removed the focused one.
    fn normalize_focus(&mut self, sensitive: bool) {
        let fields = self.fields(sensitive);
        if !fields.contains(&self.focus) {
            self.focus = fields[0];
        }
    }

    /// Moves focus to the next (`forward`) or previous control, wrapping over [`Self::fields`].
    fn step_focus(&mut self, sensitive: bool, forward: bool) {
        let fields = self.fields(sensitive);
        let len = fields.len();
        let current = fields.iter().position(|&f| f == self.focus).unwrap_or(0);
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.focus = fields[next];
    }

    /// The live match-indicator state, silent until both fields have content (#61).
    pub(crate) fn match_state(&self) -> MatchState {
        let p = self.passphrase.expose_secret();
        let c = self.confirm.expose_secret();
        if p.is_empty() || c.is_empty() {
            MatchState::Silent
        } else if p == c {
            MatchState::Match
        } else {
            MatchState::Mismatch
        }
    }

    /// Validates the buffers for a forward move (see [`Commit`]). Reads the secrets only to
    /// classify them and, on success, hands back a fresh `SecretString` — no plaintext escapes.
    pub(crate) fn commit(&self) -> Commit {
        let p = self.passphrase.expose_secret();
        if p.is_empty() {
            return Commit::Empty;
        }
        if p != self.confirm.expose_secret() {
            return Commit::Mismatch;
        }
        Commit::Passphrase(SecretString::from(p.to_owned()))
    }

    /// Whether the offered opt-in has chosen to encrypt (drives the forward path in offered
    /// mode).
    pub(crate) fn encrypt_opt_in(&self) -> bool {
        self.encrypt_opt_in
    }

    /// Handles a key while the modal is open — it is fully captive, so every key routes here.
    /// Returns what the wizard must do; the modal closes itself on cancel/confirm.
    pub(crate) fn handle_modal_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let outcome = {
            let Some(modal) = self.modal.as_mut() else {
                return ModalOutcome::Stay;
            };
            match key.code {
                KeyCode::Esc => ModalOutcome::Closed,
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    modal.toggle_focus();
                    ModalOutcome::Stay
                }
                KeyCode::Backspace => {
                    modal.typed.pop();
                    ModalOutcome::Stay
                }
                KeyCode::Enter => match modal.focus {
                    ModalButton::Cancel => ModalOutcome::Closed,
                    ModalButton::Confirm if modal.confirm_enabled() => {
                        ModalOutcome::ConfirmPlaintext
                    }
                    ModalButton::Confirm => ModalOutcome::Stay,
                },
                KeyCode::Char(c) => {
                    modal.typed.push(c);
                    ModalOutcome::Stay
                }
                _ => ModalOutcome::Stay,
            }
        };
        if matches!(
            outcome,
            ModalOutcome::Closed | ModalOutcome::ConfirmPlaintext
        ) {
            self.modal = None;
        }
        outcome
    }

    /// Handles a plain (non-chord) key against the focused control. Returns whether the key was
    /// consumed — an unconsumed key (e.g. a digit while focus is a non-text control) falls back
    /// to the shell's `1`–`4` step-jump, so digit-jump is suppressed exactly while a text field
    /// has focus (#51).
    pub(crate) fn handle_plain_key(&mut self, key: KeyEvent, sensitive: bool) -> bool {
        self.normalize_focus(sensitive);
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                self.step_focus(sensitive, true);
                true
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.step_focus(sensitive, false);
                true
            }
            KeyCode::Left if self.focus == Field::Radio => {
                self.encrypt_opt_in = false;
                self.normalize_focus(sensitive);
                true
            }
            KeyCode::Right if self.focus == Field::Radio => {
                self.encrypt_opt_in = true;
                true
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.focus == Field::Radio => {
                self.encrypt_opt_in = !self.encrypt_opt_in;
                self.normalize_focus(sensitive);
                true
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.focus == Field::Disable => {
                self.modal = Some(PlaintextModal::new());
                true
            }
            KeyCode::Backspace => match self.focus {
                Field::Passphrase => {
                    self.passphrase = pop_char(&self.passphrase);
                    true
                }
                Field::Confirm => {
                    self.confirm = pop_char(&self.confirm);
                    true
                }
                _ => false,
            },
            KeyCode::Char(c) => match self.focus {
                Field::Passphrase => {
                    self.passphrase = push_char(&self.passphrase, c);
                    true
                }
                Field::Confirm => {
                    self.confirm = push_char(&self.confirm, c);
                    true
                }
                // Not a text field: leave the digit for the shell's step-jump.
                _ => false,
            },
            _ => false,
        }
    }
}

/// Rebuilds a secret with `c` appended. The intermediate `String` is moved straight into a new
/// `SecretString` (zeroized on drop) — no plaintext copy is retained.
fn push_char(secret: &SecretString, c: char) -> SecretString {
    let mut buf = secret.expose_secret().to_owned();
    buf.push(c);
    SecretString::from(buf)
}

/// Rebuilds a secret with its last character removed.
fn pop_char(secret: &SecretString) -> SecretString {
    let mut buf = secret.expose_secret().to_owned();
    buf.pop();
    SecretString::from(buf)
}

/// Renders one passphrase field as `label [ content ]  (hint)`. When `reveal` is set the
/// content is `expose_secret()` formatted for this frame only; otherwise a bullet mask. The
/// returned line borrows nothing from the secret — the revealed text is an owned `String` that
/// lives no longer than the frame.
fn field_line(
    label: &str,
    secret: &SecretString,
    focused: bool,
    reveal: bool,
    glyphs: Glyphs,
) -> Line<'static> {
    let content = if reveal {
        secret.expose_secret().to_owned()
    } else {
        "•".repeat(secret.expose_secret().chars().count())
    };
    let marker = if focused {
        format!("{} ", glyphs.pointer)
    } else {
        "  ".to_owned()
    };
    let label_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(marker),
        Span::styled(format!("{label:<11}"), label_style),
        Span::raw(format!("[ {content} ]")),
    ])
}

/// The match-indicator line for the current state, or an empty line when silent (#61).
fn match_line(state: MatchState, glyphs: Glyphs) -> Line<'static> {
    match state {
        MatchState::Silent => Line::from(""),
        MatchState::Match => Line::from(Span::styled(
            format!("  {} passphrases match", glyphs.check),
            Style::default().fg(Color::Green),
        )),
        MatchState::Mismatch => Line::from(Span::styled(
            format!("  {} passphrases do not match", glyphs.cross),
            Style::default().fg(Color::Red),
        )),
    }
}

/// The reveal hint that rides the passphrase field, toggling with the reveal state (#61 / #51).
fn reveal_hint(reveal: bool) -> Span<'static> {
    let text = if reveal {
        "  (F2 hide)"
    } else {
        "  (F2 reveal)"
    };
    Span::styled(text, Style::default().add_modifier(Modifier::DIM))
}

/// Draws the encryption step body, then the plaintext modal on top when it is open. `reveal`
/// is the view-only `App` flag `F2` toggles.
pub(crate) fn render(frame: &mut Frame, area: Rect, wizard: &Wizard, reveal: bool, glyphs: Glyphs) {
    let state = wizard.state();
    let screen = wizard.encryption_screen();
    let sensitive = state.is_sensitive();
    let categories = crypto::evaluate(state.selection()).sensitive_categories;

    let mut lines: Vec<Line<'static>> = Vec::new();

    if sensitive {
        lines.push(Line::from(Span::styled(
            "Encryption",
            Style::default().add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Encryption (optional)",
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }

    if state.encryption_invalidated() {
        lines.push(Line::from(Span::styled(
            format!("{} {INVALIDATION_BANNER}", glyphs.warn),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }

    if sensitive {
        lines.push(Line::from(
            "This selection contains Sensitive content, so the whole Archive will be encrypted.",
        ));
        lines.push(Line::from(format!(
            "Sensitive Categories: {}",
            categories.join(", ")
        )));
        lines.push(Line::from(""));
        push_passphrase_fields(&mut lines, screen, reveal, glyphs);
        lines.push(Line::from(""));
        let disable_focused = screen.focus == Field::Disable;
        let marker = if disable_focused {
            format!("{} ", glyphs.pointer)
        } else {
            "  ".to_owned()
        };
        let style = if disable_focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled("Or: [ disable encryption (plaintext) ]", style),
        ]));
    } else {
        lines.push(Line::from(
            "Nothing here is Sensitive. Leave it plaintext, or set a passphrase to encrypt anyway.",
        ));
        lines.push(Line::from(""));
        let radio_focused = screen.focus == Field::Radio;
        lines.push(radio_line(
            !screen.encrypt_opt_in,
            radio_focused && !screen.encrypt_opt_in,
            "Leave plaintext  (default)",
            glyphs,
        ));
        lines.push(radio_line(
            screen.encrypt_opt_in,
            radio_focused && screen.encrypt_opt_in,
            "Encrypt with a passphrase",
            glyphs,
        ));
        if screen.encrypt_opt_in {
            push_passphrase_fields(&mut lines, screen, reveal, glyphs);
        }
    }

    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(body, area);

    if let Some(modal) = &screen.modal {
        render_modal(frame, area, modal, &categories);
    }
}

/// Pushes the passphrase + confirm fields and the match indicator onto `lines`.
fn push_passphrase_fields(
    lines: &mut Vec<Line<'static>>,
    screen: &EncryptionScreen,
    reveal: bool,
    glyphs: Glyphs,
) {
    let mut passphrase = field_line(
        "Passphrase",
        &screen.passphrase,
        screen.focus == Field::Passphrase,
        reveal,
        glyphs,
    );
    passphrase.spans.push(reveal_hint(reveal));
    lines.push(passphrase);
    lines.push(field_line(
        "Confirm",
        &screen.confirm,
        screen.focus == Field::Confirm,
        reveal,
        glyphs,
    ));
    lines.push(match_line(screen.match_state(), glyphs));
}

/// One offered-mode radio row: `( )`/`(•)` selection glyph plus the label, marked when focused.
fn radio_line(selected: bool, focused: bool, label: &str, glyphs: Glyphs) -> Line<'static> {
    let marker = if focused {
        format!("{} ", glyphs.pointer)
    } else {
        "  ".to_owned()
    };
    let glyph = if selected { "(•) " } else { "( ) " };
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(marker),
        Span::styled(format!("{glyph}{label}"), style),
    ])
}

/// Draws the plaintext second-confirmation modal centered over the body (#61 copy sheet).
fn render_modal(
    frame: &mut Frame,
    area: Rect,
    modal: &PlaintextModal,
    categories: &[&'static str],
) {
    let named = categories
        .iter()
        .map(|c| format!("• {c}"))
        .collect::<Vec<_>>()
        .join("  ");

    let cancel_style = if modal.focus == ModalButton::Cancel {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
    };
    let confirm_style = if !modal.confirm_enabled() {
        Style::default().add_modifier(Modifier::DIM)
    } else if modal.focus == ModalButton::Confirm {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
    };

    let lines = vec![
        Line::from("These Sensitive Categories will be written unencrypted:"),
        Line::from(format!("  {named}")),
        Line::from(""),
        Line::from(format!(
            "Type  {PLAINTEXT_WORD}  to confirm:  [ {} ]",
            modal.typed
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("       "),
            Span::styled("[ Cancel ]", cancel_style),
            Span::raw("     "),
            Span::styled("[ Confirm ]", confirm_style),
        ]),
    ];

    let modal_area = centered_rect(70, 9, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Write credentials in plaintext?")
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(Clear, modal_area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        modal_area,
    );
}

/// A `width`×`height` rectangle centered in `area`, clamped to fit.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let [horizontal] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(area);
    let [vertical] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(horizontal);
    vertical
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn type_str(screen: &mut EncryptionScreen, sensitive: bool, s: &str) {
        for c in s.chars() {
            assert!(screen.handle_plain_key(key(KeyCode::Char(c)), sensitive));
        }
    }

    #[test]
    fn typing_into_the_passphrase_field_builds_a_secret_never_a_plain_string() {
        let mut screen = EncryptionScreen::default();
        // Required mode: Passphrase is the first field.
        type_str(&mut screen, true, "hunter2");
        assert_eq!(screen.passphrase.expose_secret(), "hunter2");
        // Backspace shortens the secret.
        assert!(screen.handle_plain_key(key(KeyCode::Backspace), true));
        assert_eq!(screen.passphrase.expose_secret(), "hunter");
    }

    #[test]
    fn match_indicator_is_silent_until_both_fields_have_content() {
        let mut screen = EncryptionScreen::default();
        assert_eq!(screen.match_state(), MatchState::Silent);
        type_str(&mut screen, true, "abc");
        // Confirm still empty → silent.
        assert_eq!(screen.match_state(), MatchState::Silent);
        // Move to confirm and type a differing value → mismatch.
        assert!(screen.handle_plain_key(key(KeyCode::Tab), true));
        type_str(&mut screen, true, "abd");
        assert_eq!(screen.match_state(), MatchState::Mismatch);
        // Fix the last char → match.
        assert!(screen.handle_plain_key(key(KeyCode::Backspace), true));
        type_str(&mut screen, true, "c");
        assert_eq!(screen.match_state(), MatchState::Match);
    }

    #[test]
    fn commit_classifies_empty_mismatch_and_valid_passphrases() {
        let mut screen = EncryptionScreen::default();
        assert!(matches!(screen.commit(), Commit::Empty));

        type_str(&mut screen, true, "abc");
        screen.handle_plain_key(key(KeyCode::Tab), true);
        type_str(&mut screen, true, "abd");
        assert!(matches!(screen.commit(), Commit::Mismatch));

        screen.handle_plain_key(key(KeyCode::Backspace), true);
        type_str(&mut screen, true, "c");
        match screen.commit() {
            Commit::Passphrase(secret) => assert_eq!(secret.expose_secret(), "abc"),
            other => panic!("expected a committed passphrase, got {other:?}"),
        }
    }

    #[test]
    fn offered_mode_opt_in_reveals_the_passphrase_fields() {
        let mut screen = EncryptionScreen::default();
        // Non-Sensitive: focus normalizes to the radio (the only field) and defaults plaintext.
        screen.normalize_focus(false);
        assert_eq!(screen.focus, Field::Radio);
        assert!(!screen.encrypt_opt_in());
        // Choosing encrypt exposes the passphrase fields as focusable.
        assert!(screen.handle_plain_key(key(KeyCode::Right), false));
        assert!(screen.encrypt_opt_in());
        assert_eq!(
            screen.fields(false),
            vec![Field::Radio, Field::Passphrase, Field::Confirm]
        );
    }

    #[test]
    fn a_digit_types_into_a_text_field_but_is_left_for_the_shell_otherwise() {
        let mut screen = EncryptionScreen::default();
        // Focus is the passphrase field → the digit is consumed (typed), so the shell must not
        // treat it as a step jump (#51 suppression).
        assert!(screen.handle_plain_key(key(KeyCode::Char('2')), true));
        assert_eq!(screen.passphrase.expose_secret(), "2");

        // Offered mode, focus on the radio (not a text field) → the digit is *not* consumed,
        // so it falls through to the shell's step-jump.
        let mut radio = EncryptionScreen::default();
        radio.normalize_focus(false);
        assert!(!radio.handle_plain_key(key(KeyCode::Char('2')), false));
    }

    #[test]
    fn modal_confirm_enables_only_on_exact_plaintext_and_defaults_to_cancel() {
        let mut modal = PlaintextModal::new();
        assert_eq!(modal.focus, ModalButton::Cancel);
        assert!(!modal.confirm_enabled());
        for c in "PLAINTEX".chars() {
            modal.typed.push(c);
        }
        assert!(!modal.confirm_enabled());
        modal.typed.push('T');
        assert!(modal.confirm_enabled());
        // Lowercase / extra text does not count.
        modal.typed.push('s');
        assert!(!modal.confirm_enabled());
    }

    /// A screen with the plaintext modal already open.
    fn screen_with_modal() -> EncryptionScreen {
        EncryptionScreen {
            modal: Some(PlaintextModal::new()),
            ..EncryptionScreen::default()
        }
    }

    #[test]
    fn modal_key_flow_confirms_only_when_enabled_and_focused() {
        let mut screen = screen_with_modal();
        assert!(screen.is_modal_open());

        // Enter on the default-focused Cancel closes without confirming.
        let outcome = screen.handle_modal_key(key(KeyCode::Enter));
        assert_eq!(outcome, ModalOutcome::Closed);
        assert!(!screen.is_modal_open());

        // Reopen, type the word, focus Confirm, and confirm.
        screen.modal = Some(PlaintextModal::new());
        type_str_modal(&mut screen, PLAINTEXT_WORD);
        // Tab to Confirm.
        assert_eq!(
            screen.handle_modal_key(key(KeyCode::Tab)),
            ModalOutcome::Stay
        );
        let outcome = screen.handle_modal_key(key(KeyCode::Enter));
        assert_eq!(outcome, ModalOutcome::ConfirmPlaintext);
        assert!(!screen.is_modal_open());
    }

    fn type_str_modal(screen: &mut EncryptionScreen, s: &str) {
        for c in s.chars() {
            assert_eq!(
                screen.handle_modal_key(key(KeyCode::Char(c))),
                ModalOutcome::Stay
            );
        }
    }

    #[test]
    fn confirm_with_wrong_text_does_not_confirm() {
        let mut screen = screen_with_modal();
        type_str_modal(&mut screen, "plaintext"); // wrong case
        screen.handle_modal_key(key(KeyCode::Tab)); // focus Confirm
        assert_eq!(
            screen.handle_modal_key(key(KeyCode::Enter)),
            ModalOutcome::Stay
        );
        assert!(screen.is_modal_open());
    }

    #[test]
    fn disable_button_opens_the_modal_in_required_mode() {
        let mut screen = EncryptionScreen::default();
        // Tab from Passphrase → Confirm → Disable.
        screen.handle_plain_key(key(KeyCode::Tab), true);
        screen.handle_plain_key(key(KeyCode::Tab), true);
        assert_eq!(screen.focus, Field::Disable);
        assert!(screen.handle_plain_key(key(KeyCode::Enter), true));
        assert!(screen.is_modal_open());
    }
}
