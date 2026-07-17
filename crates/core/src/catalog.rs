//! The ten Category definitions (SSH, GPG, Cloud creds, KDE Plasma, Agents, Shell,
//! Program configs, Package lists, User data, Custom paths) and each Category's Curated
//! allowlist of Entries.
//!
//! Category precedence (high to low, for an Entry matching more than one allowlist):
//! SSH · GPG · Cloud-creds -> KDE Plasma -> Agents -> Shell -> Program configs ->
//! Package lists -> User data -> Custom paths.
//!
//! #19 scope: only the **Shell** Category is implemented, with just enough of a Curated
//! allowlist (`oh-my-zsh/custom/`) to drive the round-trip seam end-to-end. The full ten-
//! Category taxonomy (precedence resolution across overlapping allowlists, the other nine
//! Categories, Hide-empty) is #20's job — nothing here should be mistaken for that.

/// A named group a path can belong to, backed by a Curated allowlist. See CONTEXT.md's
/// "Category" entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    /// The Category's name, used verbatim in the Manifest's `entries[].category` field.
    pub name: &'static str,
    /// Whether every Entry in this Category is Sensitive (a "sensitive Category" origin,
    /// per CONTEXT.md's "Sensitive" entry). Per-entry sensitivity tags within an otherwise
    /// non-sensitive Category are out of scope for #19 (see #22).
    pub sensitive: bool,
    /// The Curated allowlist: paths relative to `$HOME` that make up this Category's
    /// Entries. Each may be a file or a directory (directories are walked recursively by
    /// `scan::scan_category`).
    pub allowlist: &'static [&'static str],
}

/// The Shell Category: `oh-my-zsh/custom/` only, never the whole oh-my-zsh framework
/// (story: "Shell (`oh-my-zsh/custom/` only, not the whole framework)").
pub const SHELL: Category = Category {
    name: "Shell",
    sensitive: false,
    allowlist: &[".oh-my-zsh/custom"],
};
