//! The shared fish-style path-autocomplete engine for the custom-path fields (#52 / #75).
//!
//! Both the WHERE Output-location field ([`crate::where_step`]) and the WHAT Custom-path add
//! field ([`crate::what_screen`]) drive the same completion behaviour — a live candidate list,
//! inline ghost-text, component-by-component `Tab`, and `→`/`End` accept — over the same policy:
//! completion is over **existing paths only** (the tool never creates), dotfiles stay hidden
//! until the current segment starts with `.`, and non-root scope is proactive and lexical
//! (matching [`catalog::validate_custom_path`]). The two screens differ only in what a *committed*
//! path must be (WHERE requires a writable directory; WHAT accepts an existing file or directory),
//! so that acceptance policy stays in each screen while the engine itself lives here once — the
//! second real caller ADR-0001's reasoning was waiting for.
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

/// How many candidate rows a field shows before folding the remainder into a `+M more` footer
/// (#52's type-to-narrow, no giant scroll). Kept small so a list fits without a scrollbar.
pub(crate) const CANDIDATE_CAP: usize = 8;

/// The computed autocomplete listing for a buffer — recomputed on demand (in both the reducer
/// and the render) rather than cached, so it always reflects the live buffer and disk.
pub(crate) struct Autocomplete {
    pub candidates: Vec<DirEntry>,
    pub selected: usize,
    /// Total matches before the [`CANDIDATE_CAP`] truncation, for the `+M more` footer.
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
    matched.truncate(CANDIDATE_CAP);
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

/// `Tab`: complete the longest common prefix, or descend into an unambiguous single directory —
/// component-by-component, stopping at each directory's trailing `/` (#52). Mutates `buffer`; the
/// caller resets the candidate cursor.
pub(crate) fn complete_tab(buffer: &mut String, view: &Autocomplete) {
    if view.candidates.is_empty() {
        return;
    }
    let next = {
        let (dir_part, fragment) = split_buffer(buffer);
        if view.candidates.len() == 1 {
            Some(extend(dir_part, &view.candidates[0]))
        } else {
            let lcp = longest_common_prefix(&view.candidates);
            // Only extend when the common prefix reaches past what's already typed — otherwise
            // Tab is a no-op on an already-ambiguous fragment (the prototype's `~/Do →Tab→ ~/Do`).
            if lcp.chars().count() > fragment.chars().count() {
                Some(format!("{dir_part}{lcp}"))
            } else {
                None
            }
        }
    };
    if let Some(next) = next {
        *buffer = next;
    }
}

/// `→`/`End`: accept the whole ghost — the highlighted candidate, eagerly (fish-style), with a
/// trailing `/` when it's a directory so the next segment can follow. Mutates `buffer`; the
/// caller resets the candidate cursor.
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

/// The longest common prefix of the candidates' names — how far `Tab` can extend a fragment
/// without guessing between them.
fn longest_common_prefix(candidates: &[DirEntry]) -> String {
    let mut names = candidates.iter().map(|c| c.name.to_string_lossy());
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for name in names {
        let common = prefix
            .iter()
            .zip(name.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(common);
    }
    prefix.into_iter().collect()
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
/// cursored) with a `+M more` footer when the list overflows [`CANDIDATE_CAP`]. The caller owns
/// the surrounding frame (title, field line, per-screen warnings, scope hint); this is only the
/// common list the two screens render identically.
pub(crate) fn candidate_lines(view: &Autocomplete, glyphs: Glyphs) -> Vec<Line<'static>> {
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

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (idx, candidate) in view.candidates.iter().enumerate() {
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
    let hidden = view.total.saturating_sub(view.candidates.len());
    if hidden > 0 {
        lines.push(Line::styled(
            format!("  +{hidden} more — keep typing to narrow"),
            dim,
        ));
    }
    lines
}
