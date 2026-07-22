//! The shared fish-style path-autocomplete engine for the custom-path fields (#52 / #75 / #84).
//!
//! Both the WHERE Output-location field ([`crate::where_step`]) and the WHAT Custom-path add
//! field ([`crate::what_screen`]) drive the same completion behaviour — a live candidate list,
//! inline ghost-text, and `Tab`/`→`/`End` accept — over the same policy: completion is over
//! **existing paths only** (the tool never creates), dotfiles stay hidden until the current
//! segment starts with `.`, and non-root scope is proactive and lexical (matching
//! [`catalog::validate_custom_path`]). The two screens differ only in what a *committed* path
//! must be (WHERE requires a writable directory; WHAT accepts an existing file or directory), so
//! that acceptance policy stays in each screen while the engine itself lives here once — the
//! second real caller ADR-0001's reasoning was waiting for.
//!
//! `Tab` menu-completes: it accepts the highlighted candidate, exactly like `→`/`End` (#84). A
//! prior LCP `complete_tab` made `Tab` extend only as far as the candidates' common prefix — a
//! silent no-op once the fragment itself was the longest common prefix (`~/Do` staying `~/Do`
//! with `Documents`/`Downloads` both live). `↓` then `Tab` picks a sibling before accepting it;
//! stateful cycling on repeated `Tab` presses is deferred (needs menu state the recompute-on-
//! demand engine avoids).
//!
//! Per ADR-0005 D6 the directory listing goes through [`fs_probe::list_dir`]; per ADR-0004 the
//! `~`/`$VAR` expansion happens at the `tui` boundary through the injected `getenv` resolver,
//! never inside `core`.

use std::path::Path;

use linux_backup_core::fs_probe::{self, DirEntry};
use linux_backup_core::location;
use linux_backup_core::{catalog, Privilege};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::glyphs::Glyphs;

/// A resolver from env-var name to value — the ADR-0004 carve-out, supplied by the caller so
/// `core` never reads the environment itself. `main` passes `std::env::var`; tests pass a fake.
pub(crate) type Getenv<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The absolute ceiling on candidate rows ever kept or shown, regardless of terminal size (#84):
/// past this, type-to-narrow — not a taller list — is expected to do the work. `compute` truncates
/// to this once per keystroke; [`candidate_cap`] then narrows further for a short pane.
pub(crate) const MAX_CANDIDATE_CAP: usize = 10;

/// The adaptive display cap for a pane `body_rows` tall: two thirds of the available rows, up to
/// [`MAX_CANDIDATE_CAP`] (#84). Display-only — `compute` already did the full listing, filtering,
/// and sorting up to [`MAX_CANDIDATE_CAP`], so narrowing the *shown* slice at render time adds no
/// new per-keystroke cost.
pub(crate) fn candidate_cap(body_rows: usize) -> usize {
    (body_rows * 2 / 3).min(MAX_CANDIDATE_CAP)
}

/// The computed autocomplete listing for a buffer — recomputed on demand (in both the reducer
/// and the render) rather than cached, so it always reflects the live buffer and disk.
pub(crate) struct Autocomplete {
    pub candidates: Vec<DirEntry>,
    pub selected: usize,
    /// Total matches before the [`MAX_CANDIDATE_CAP`] truncation, for the `+M more` footer.
    pub total: usize,
    /// Non-root typed a path outside `$HOME` — no candidates are offered (proactive scope, #52).
    pub out_of_scope: bool,
}

impl Autocomplete {
    /// The empty listing: an unopened field, or a buffer with no input yet (never out of scope).
    pub(crate) fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            selected: 0,
            total: 0,
            out_of_scope: false,
        }
    }
}

/// Computes the live candidate listing for `buffer`, applying scope, dotfile hiding, prefix
/// narrowing, and dirs-first ordering. The directory listing itself is `core`'s (ADR-0005 D6);
/// everything here is presentation/policy over what it returns.
pub(crate) fn compute(
    buffer: &str,
    ac_selected: usize,
    show_hidden: bool,
    home: &Path,
    privilege: Privilege,
    getenv: Getenv,
) -> Autocomplete {
    // An empty buffer is "no input yet", not an out-of-scope path: expanding it yields an empty
    // path that fails the non-root scope check, so without this a non-root user would see
    // `⚠ outside ~` the instant the field opens, before typing anything (#52).
    if buffer.is_empty() {
        return Autocomplete::empty();
    }

    let expanded_full = location::expand_path(buffer, home, getenv);
    if catalog::validate_custom_path(&expanded_full, home, privilege).is_err() {
        return Autocomplete {
            candidates: Vec::new(),
            selected: 0,
            total: 0,
            out_of_scope: true,
        };
    }

    let (dir_part, fragment) = split_buffer(buffer);
    let listing_dir = location::expand_path(dir_part, home, getenv);
    let fragment_lower = fragment.to_lowercase();
    let reveal_hidden = show_hidden || fragment.starts_with('.');

    let mut matched: Vec<DirEntry> = fs_probe::list_dir(&listing_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            let name = entry.name.to_string_lossy();
            if name.starts_with('.') && !reveal_hidden {
                return false;
            }
            name.to_lowercase().starts_with(&fragment_lower)
        })
        .collect();
    // Directories first, then files; alphabetical case-insensitive within each group (#52).
    matched.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            a.name
                .to_string_lossy()
                .to_lowercase()
                .cmp(&b.name.to_string_lossy().to_lowercase())
        })
    });

    let total = matched.len();
    matched.truncate(MAX_CANDIDATE_CAP);
    let selected = ac_selected.min(matched.len().saturating_sub(1));
    Autocomplete {
        candidates: matched,
        selected,
        total,
        out_of_scope: false,
    }
}

/// The ghost-text tail: the remaining characters of the highlighted candidate past what's typed,
/// plus a `/` for a directory. Empty when there's nothing to suggest.
pub(crate) fn ghost(buffer: &str, view: &Autocomplete) -> String {
    let (_, fragment) = split_buffer(buffer);
    let Some(candidate) = view.candidates.get(view.selected) else {
        return String::new();
    };
    let name = candidate.name.to_string_lossy();
    let typed = fragment.chars().count();
    let mut ghost: String = name.chars().skip(typed).collect();
    if candidate.is_dir {
        ghost.push('/');
    }
    ghost
}

/// `Tab`/`→`/`End`: accept the whole ghost — the highlighted candidate, eagerly (fish-style, menu-
/// complete), with a trailing `/` when it's a directory so the next segment can follow (#84: all
/// three keys are one binding). Mutates `buffer`; the caller resets the candidate cursor.
pub(crate) fn accept_ghost(buffer: &mut String, view: &Autocomplete) {
    let Some(candidate) = view.candidates.get(view.selected) else {
        return;
    };
    let next = {
        let (dir_part, _) = split_buffer(buffer);
        extend(dir_part, candidate)
    };
    *buffer = next;
}

/// Splits a path buffer into its committed directory part (everything up to and including the
/// last `/`) and the current segment being typed after it. A buffer with no `/` is all segment.
pub(crate) fn split_buffer(buffer: &str) -> (&str, &str) {
    match buffer.rfind('/') {
        Some(idx) => (&buffer[..=idx], &buffer[idx + 1..]),
        None => ("", buffer),
    }
}

/// The directory part plus `candidate`'s name, with a trailing `/` for a directory so completion
/// stops at (and can continue past) the segment boundary.
fn extend(dir_part: &str, candidate: &DirEntry) -> String {
    let mut out = format!("{dir_part}{}", candidate.name.to_string_lossy());
    if candidate.is_dir {
        out.push('/');
    }
    out
}

/// The standing scope hint under a custom-path field: root ranges the whole filesystem, non-root
/// stays under `~` (#52). Shared verbatim by both fields ([`crate::where_step`], [`crate::what_screen`]).
pub(crate) fn scope_hint(privilege: Privilege) -> &'static str {
    match privilege {
        Privilege::Root => "scope: full filesystem (root)",
        Privilege::NonRoot => "scope: under ~ (non-root)",
    }
}

/// The candidate-list body both custom-path fields draw: the out-of-scope warning for a non-root
/// path outside `~`, or the dirs-first candidate rows (symlink-marked, the highlighted row
/// cursored) with a `+M more` footer when the list overflows the [`candidate_cap`] for a
/// `body_rows`-tall pane (#84). The caller owns the surrounding frame (title, field line,
/// per-screen warnings, scope hint); this is only the common list the two screens render
/// identically.
pub(crate) fn candidate_lines(
    view: &Autocomplete,
    glyphs: Glyphs,
    body_rows: usize,
) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let warn = Style::default().fg(Color::Yellow);

    if view.out_of_scope {
        return vec![Line::styled(
            format!(
                "{} outside ~ — non-root can only add paths under your home",
                glyphs.warn
            ),
            warn,
        )];
    }

    let shown = view.candidates.len().min(candidate_cap(body_rows));
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (idx, candidate) in view.candidates.iter().take(shown).enumerate() {
        let name = candidate.name.to_string_lossy();
        let slash = if candidate.is_dir { "/" } else { "" };
        let kind = if candidate.is_dir { "dir" } else { "file" };
        let link = if candidate.is_symlink {
            " →symlink"
        } else {
            ""
        };
        let cursor = if idx == view.selected { "  ←" } else { "" };
        lines.push(Line::from(format!("  {name}{slash}  {kind}{link}{cursor}")));
    }
    let hidden = view.total.saturating_sub(shown);
    if hidden > 0 {
        lines.push(Line::styled(
            format!("  +{hidden} more — keep typing to narrow"),
            dim,
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_cap_scales_with_two_thirds_of_body_rows() {
        assert_eq!(candidate_cap(9), 6);
        assert_eq!(candidate_cap(12), 8);
    }

    #[test]
    fn candidate_cap_never_exceeds_the_absolute_ceiling() {
        // A tall pane (e.g. `Comfortable` reflow) still stops at `MAX_CANDIDATE_CAP` — past it,
        // type-to-narrow does the work rather than an ever-taller list (#84).
        assert_eq!(candidate_cap(100), MAX_CANDIDATE_CAP);
    }

    #[test]
    fn candidate_cap_shrinks_to_zero_in_a_body_with_no_room() {
        assert_eq!(candidate_cap(0), 0);
        assert_eq!(candidate_cap(1), 0);
    }
}
