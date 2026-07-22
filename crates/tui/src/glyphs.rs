//! The no-emoji fallback glyph set (#51 / #61), detected once at startup and threaded through
//! every screen's render.
//!
//! A terminal without emoji/Unicode support must still render a legible UI, so the decorative
//! glyphs (`⚠`/`🔒`/`✓`/`✗`/`●`/`·`/`▸` and the braille spinner) each carry an ASCII fallback.
//! Which set is used is an **ambient run fact** — it depends on the process's locale — so per
//! ADR-0004 it is decided at the `tui` boundary (from the injected environment, never inside
//! `core`) exactly once and carried in [`crate::app::App`] as a `Copy` value threaded into the
//! pure render functions. There is deliberately no runtime toggle (#61): the choice is made at
//! startup and never changes.
//!
//! Only the glyphs listed in the #61 fallback map are remapped. Arrows (`↑↓←→`), box-drawing,
//! and the `·` **separators** between footer/stepper hints stay Unicode in both sets — a `·`
//! remaps to `.` only where it is a semantic *glyph* (the locked-step / pending-phase marker),
//! which is why the set carries a named `locked` field rather than blindly substituting the
//! character everywhere it appears.

/// The semantic glyphs a screen draws, each resolved to the active set's rendering. Copy so it
/// threads into render functions by value without lifetimes; every field is `'static` because
/// both sets are compile-time constants.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    /// Warning marker (`⚠` → `/!\`): advisory space, out-of-scope, plaintext, vanished.
    pub warn: &'static str,
    /// Sensitive lock (`🔒` → `[!]`): the Encryption stepper label and Category rows.
    pub lock: &'static str,
    /// Success/positive (`✓` → `OK`): stepper completed, match indicator, writable, complete.
    pub check: &'static str,
    /// Failure (`✗` → `x`): mismatch, failed run/restore, wrong passphrase.
    pub cross: &'static str,
    /// Current marker (`●` → `>`): the stepper's current step and the RUN phase in progress.
    pub current: &'static str,
    /// Locked/pending marker (`·` → `.`): the stepper's locked steps and pending RUN phases.
    /// Distinct from the `·` used as a separator, which is left Unicode.
    pub locked: &'static str,
    /// Focus pointer (`▸` → `>`): pane cursors, field markers. The WHAT breadcrumb's `/`
    /// separator is a literal character, decoupled from this glyph (#85).
    pub pointer: &'static str,
    /// Bullet (`•` → `*`): the passphrase mask, the offered/pick-list radio dot, the
    /// plaintext-modal Category list, and the restore partial-report failure lines.
    pub bullet: &'static str,
    /// The indeterminate-phase spinner frames (braille → `\|/-`).
    pub spinner: &'static [char],
}

/// The braille spinner frames used when Unicode is available.
const UNICODE_SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠇'];

/// The ASCII spinner frames used under the no-emoji fallback.
const ASCII_SPINNER: [char; 4] = ['\\', '|', '/', '-'];

impl Glyphs {
    /// The full-Unicode set — the default on a modern UTF-8 terminal.
    pub const UNICODE: Self = Self {
        warn: "⚠",
        lock: "🔒",
        check: "✓",
        cross: "✗",
        current: "●",
        locked: "·",
        pointer: "▸",
        bullet: "•",
        spinner: &UNICODE_SPINNER,
    };

    /// The no-emoji fallback set (#61 map). `⚠`→`/!\`, `🔒`→`[!]`, `✓`→`OK`, `✗`→`x`, `●`→`>`,
    /// `·`→`.`, `▸`→`>`, `•`→`*`, spinner→`\|/-`.
    pub const ASCII: Self = Self {
        warn: "/!\\",
        lock: "[!]",
        check: "OK",
        cross: "x",
        current: ">",
        locked: ".",
        pointer: ">",
        bullet: "*",
        spinner: &ASCII_SPINNER,
    };

    /// Chooses the set from the process locale, read through an injected getter so the decision
    /// stays a pure function of its inputs (ADR-0004: the ambient env read happens at the `tui`
    /// boundary in `main`, not here). A locale naming UTF-8 gets the Unicode set; an explicit
    /// non-UTF-8 locale (`C`/`POSIX`/legacy 8-bit) falls back to ASCII; when no locale is set at
    /// all a modern desktop terminal is assumed and the Unicode set is used.
    pub fn detect(get: impl Fn(&str) -> Option<String>) -> Self {
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|key| get(key).filter(|value| !value.is_empty()));
        match locale {
            Some(value) => {
                let lower = value.to_ascii_lowercase();
                if lower.contains("utf-8") || lower.contains("utf8") {
                    Self::UNICODE
                } else {
                    Self::ASCII
                }
            }
            None => Self::UNICODE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn a_utf8_locale_selects_the_unicode_set() {
        let glyphs = Glyphs::detect(env(&[("LANG", "en_US.UTF-8")]));
        assert_eq!(glyphs.warn, "⚠");
        assert_eq!(glyphs.spinner.len(), 8);
    }

    #[test]
    fn a_non_utf8_locale_falls_back_to_ascii() {
        let glyphs = Glyphs::detect(env(&[("LC_ALL", "C")]));
        assert_eq!(glyphs.warn, "/!\\");
        assert_eq!(glyphs.lock, "[!]");
        assert_eq!(glyphs.check, "OK");
        assert_eq!(glyphs.bullet, "*");
        assert_eq!(glyphs.spinner, &['\\', '|', '/', '-']);
    }

    #[test]
    fn the_unicode_bullet_stays_unicode() {
        // The non-ASCII `•` survives in the Unicode set; only the fallback swaps it to `*`.
        assert_eq!(Glyphs::UNICODE.bullet, "•");
        assert_eq!(Glyphs::ASCII.bullet, "*");
    }

    #[test]
    fn lc_all_wins_over_lang() {
        // A UTF-8 LANG is overridden by an explicit C in the higher-priority LC_ALL.
        let glyphs = Glyphs::detect(env(&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")]));
        assert_eq!(glyphs.warn, "/!\\");
    }

    #[test]
    fn an_empty_value_is_ignored_in_favour_of_the_next_key() {
        let glyphs = Glyphs::detect(env(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")]));
        assert_eq!(glyphs.warn, "⚠");
    }

    #[test]
    fn no_locale_at_all_assumes_a_modern_unicode_terminal() {
        let glyphs = Glyphs::detect(|_| None);
        assert_eq!(glyphs.warn, "⚠");
    }
}
