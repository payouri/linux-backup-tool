//! The ten Category definitions (SSH, GPG, Cloud creds, KDE Plasma, Agents, Shell,
//! Program configs, Package lists, User data, Custom paths), each with its Curated
//! allowlist of Entries, precedence resolution across overlapping allowlists, and the
//! root/non-root Custom-paths validation rule.
//!
//! Category precedence (high to low, for an Entry matching more than one allowlist):
//! SSH · GPG · Cloud-creds -> KDE Plasma -> Agents -> Shell -> Program configs ->
//! Package lists -> User data -> Custom paths. [`ALL`] is declared in exactly this order,
//! so "earliest match wins" in [`resolve_category`] *is* the precedence rule — there's no
//! separate ranking table to keep in sync.

use std::path::{Component, Path, PathBuf};

use crate::context::Privilege;
use crate::error::CoreError;

/// A named group a path can belong to, backed by a Curated allowlist. See CONTEXT.md's
/// "Category" entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    /// The Category's name, used verbatim in the Manifest's `entries[].category` field.
    pub name: &'static str,
    /// Whether every Entry in this Category is Sensitive (a "sensitive Category" origin,
    /// per CONTEXT.md's "Sensitive" entry). `scan::scan_category`/`scan_taxonomy` OR this
    /// with [`Category::sensitive_entries`] (the per-entry tag origin) and the per-Entry
    /// Safety-net scan result (#22) — this field alone is never the whole story for a given
    /// Entry's final `sensitive` bool.
    pub sensitive: bool,
    /// Whether this Category starts checked (Smart default, story 2). Sensitive Categories
    /// (SSH/GPG/Cloud-creds) and the everyday non-bulk Categories default ON; User data
    /// (bulk XDG dirs) defaults OFF; Custom paths has nothing to default since it starts
    /// empty.
    pub smart_default: bool,
    /// The Curated allowlist: paths relative to `$HOME` that make up this Category's
    /// Entries. Each may be a file or a directory (directories are walked recursively by
    /// `scan::scan_category`/`scan::scan_taxonomy`). Empty for Custom paths (always empty
    /// at start) and for Package lists (its content is captured via `packages::capture_all`
    /// straight into the Manifest, not via filesystem paths — see that module's doc).
    pub allowlist: &'static [&'static str],
    /// The subset of `allowlist` roots that are individually Sensitive even though the
    /// Category as a whole is not (CONTEXT.md's "Sensitive" entry: "a per-entry sensitivity
    /// tag (e.g. some Agents Entries)" — a third origin distinct from both the Category-level
    /// `sensitive` flag and the Safety-net scan). Each entry here must also appear in
    /// `allowlist`; an Entry is individually-tagged-sensitive if its relative path starts
    /// with one of these roots (`entry_is_individually_sensitive`). Empty for every Category
    /// with no such finer-grained tagging.
    pub sensitive_entries: &'static [&'static str],
    /// Whether this Category is exempt from Hide-empty (shown even with zero matching
    /// paths). True only for Custom paths — the always-available escape hatch (story 6).
    pub always_shown: bool,
}

/// SSH: the whole `~/.ssh` — keys, `config`, `known_hosts`, `authorized_keys`. Sensitive,
/// default-ON (story 3).
pub const SSH: Category = Category {
    name: "SSH",
    sensitive: true,
    smart_default: true,
    allowlist: &[".ssh"],
    sensitive_entries: &[],
    always_shown: false,
};

/// GPG: the whole `~/.gnupg`. Sensitive, default-ON (story 3).
pub const GPG: Category = Category {
    name: "GPG",
    sensitive: true,
    smart_default: true,
    allowlist: &[".gnupg"],
    sensitive_entries: &[],
    always_shown: false,
};

/// Cloud creds: the credential directories of the common CLI-driven cloud providers.
/// Sensitive, default-ON (story 3).
pub const CLOUD_CREDS: Category = Category {
    name: "Cloud creds",
    sensitive: true,
    smart_default: true,
    allowlist: &[".aws", ".config/gcloud", ".azure"],
    sensitive_entries: &[],
    always_shown: false,
};

/// KDE Plasma: raw config copy, not `konsave` (ADR-0003). Seeded from konsave's file set
/// plus `~/.config/kdedefaults` (which konsave misses); `icons/`/`wallpapers/` are on the
/// allowlist but heavy (~226M) and meant to be opted out per-Entry via Drill-down (#21),
/// not excluded here.
pub const KDE_PLASMA: Category = Category {
    name: "KDE Plasma",
    sensitive: false,
    smart_default: true,
    allowlist: &[
        ".config/kdedefaults",
        ".config/plasma-org.kde.plasma.desktop-appletsrc",
        ".config/plasmarc",
        ".config/kwinrc",
        ".config/kdeglobals",
        ".config/kscreenlockerrc",
        ".local/share/plasma",
        ".local/share/icons",
        ".local/share/wallpapers",
    ],
    sensitive_entries: &[],
    always_shown: false,
};

/// Agents: AI-agent tool state (`.claude`/`.agents`/`.mcp-auth`). The Category itself is not
/// blanket-Sensitive, but `.mcp-auth` (MCP server auth tokens/sessions) is individually
/// sensitivity-tagged (CONTEXT.md's "Sensitive" entry: "a per-entry sensitivity tag (e.g.
/// some Agents Entries)") — a declarative tag, not a hope that the Safety-net scan's
/// keyword/extension list happens to match whatever a given MCP server names its auth
/// files.
pub const AGENTS: Category = Category {
    name: "Agents",
    sensitive: false,
    smart_default: true,
    allowlist: &[".claude", ".agents", ".mcp-auth"],
    sensitive_entries: &[".mcp-auth"],
    always_shown: false,
};

/// Shell: `oh-my-zsh/custom/` only, never the whole oh-my-zsh framework (story: "Shell
/// (`oh-my-zsh/custom/` only, not the whole framework)"). Unchanged from #19.
pub const SHELL: Category = Category {
    name: "Shell",
    sensitive: false,
    smart_default: true,
    allowlist: &[".oh-my-zsh/custom"],
    sensitive_entries: &[],
    always_shown: false,
};

/// Program configs: the catch-all floor — a curated `~/.config` subset, top-level
/// dotfiles, and user fonts. Curated (not a whole-`~/.config` copy): only entries actually
/// listed here are captured.
pub const PROGRAM_CONFIGS: Category = Category {
    name: "Program configs",
    sensitive: false,
    smart_default: true,
    allowlist: &[
        ".bashrc",
        ".zshrc",
        ".profile",
        ".gitconfig",
        ".vimrc",
        ".tmux.conf",
        ".config/nvim",
        ".config/alacritty",
        ".config/git",
        ".local/share/fonts",
    ],
    sensitive_entries: &[],
    always_shown: false,
};

/// Package lists: per-package-manager inventories. Deliberately has **no** filesystem
/// allowlist — its content is captured by `packages::capture_all` straight into the
/// Manifest's `packages` map (#25), not discovered by `scan::scan_taxonomy`'s path-walk.
/// It still occupies its documented precedence slot (so a real overlap, were one ever
/// added, would resolve the same predictable way as every other Category) and is still one
/// of the ten Categories the user can reason about/toggle, even though today its own
/// allowlist can never produce an Entry.
pub const PACKAGE_LISTS: Category = Category {
    name: "Package lists",
    sensitive: false,
    smart_default: true,
    allowlist: &[],
    sensitive_entries: &[],
    always_shown: false,
};

/// User data: bulk XDG directories. Default-OFF with a size warning (story 4) — the one
/// large-data Category the Environment content boundary otherwise excludes.
pub const USER_DATA: Category = Category {
    name: "User data",
    sensitive: false,
    smart_default: false,
    allowlist: &[
        "Documents",
        "Downloads",
        "Pictures",
        "Videos",
        "Music",
        "Desktop",
        "Public",
        "Templates",
    ],
    sensitive_entries: &[],
    always_shown: false,
};

/// Custom paths: always empty at start, always shown even when empty (story 6) — the one
/// predictable escape hatch for an Entry not covered by any other Category's allowlist
/// (story 8).
pub const CUSTOM_PATHS: Category = Category {
    name: "Custom paths",
    sensitive: false,
    smart_default: false,
    allowlist: &[],
    sensitive_entries: &[],
    always_shown: true,
};

/// Every Category, in precedence order (high to low). This ordering is the single source
/// of truth for both "which Category wins an overlapping path" ([`resolve_category`]) and
/// "in what order does the Selection surface list Categories" — one array, not two things
/// that could drift apart.
pub const ALL: [Category; 10] = [
    SSH,
    GPG,
    CLOUD_CREDS,
    KDE_PLASMA,
    AGENTS,
    SHELL,
    PROGRAM_CONFIGS,
    PACKAGE_LISTS,
    USER_DATA,
    CUSTOM_PATHS,
];

/// Whether `relative_path` (already relative to `$HOME`) falls under one of `category`'s
/// allowlist roots.
fn category_matches(category: &Category, relative_path: &Path) -> bool {
    category
        .allowlist
        .iter()
        .any(|root| relative_path.starts_with(root))
}

/// Whether `relative_path` falls under one of `category`'s [`Category::sensitive_entries`]
/// roots — the per-entry sensitivity tag origin (CONTEXT.md's "Sensitive" entry), distinct
/// from both `category.sensitive` (the whole-Category origin) and the Safety-net scan.
/// Independent of `category_matches`: a Category's `sensitive_entries` is always a subset of
/// its `allowlist`, so this only needs to be consulted for a path already resolved to
/// `category`.
pub fn entry_is_individually_sensitive(category: &Category, relative_path: &Path) -> bool {
    category
        .sensitive_entries
        .iter()
        .any(|root| relative_path.starts_with(root))
}

/// Resolves which Category owns `relative_path` when it may match more than one entry in
/// `categories`: the first (i.e. highest-precedence) Category in `categories` whose
/// allowlist contains it. Callers pass Categories already in precedence order (e.g.
/// [`ALL`]) — this function does not re-sort or otherwise interpret ordering, it just picks
/// the first match, which is what makes "higher-precedence wins" a one-line rule instead of
/// a comparison function.
///
/// Returns the winner's index in `categories` alongside the Category so callers keying
/// parallel per-Category state by position don't have to search for it again.
///
/// Returns `None` if `relative_path` matches no Category's allowlist at all (story 8: such
/// a path is only addable via Custom paths, never silently attributed to one of the ten).
pub fn resolve_category<'a>(
    relative_path: &Path,
    categories: &'a [Category],
) -> Option<(usize, &'a Category)> {
    categories
        .iter()
        .enumerate()
        .find(|(_, category)| category_matches(category, relative_path))
}

/// Collapses `.` and `..` components purely lexically, without touching the filesystem —
/// no ambient reads (ADR-0004), no requirement that the path exist, and no CWD dependency.
/// A `..` at (or above) a filesystem root is dropped, since there is nothing above the root; a
/// leading `..` with nothing left to pop is retained, since it is a genuine "up" we can't discard.
///
/// This is deliberately *not* `std::fs::canonicalize`: symlinks are left unresolved, so a path
/// that lexically stays under `home` is treated as under `home` even if one of its parents is a
/// symlink pointing elsewhere. Resolving that would require reading the real filesystem, which
/// `core` must not do; the residual symlink-escape is out of scope here (see #44).
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => normalized.push(component),
            },
            other => normalized.push(other),
        }
    }
    normalized
}

/// Validates a Custom path against the root/non-root constraint (stories 10/11): root may
/// add any filesystem path; non-root is constrained to under `home`. `privilege` is always
/// the injected [`Privilege`] (ADR-0004) — this function never reads the real euid itself.
///
/// The non-root check normalizes `candidate` lexically first (#44): without it a traversal
/// such as `~/../etc/passwd` lexically "starts with" `home` and slips through before its `..`
/// components walk back out.
pub fn validate_custom_path(
    candidate: &Path,
    home: &Path,
    privilege: Privilege,
) -> Result<(), CoreError> {
    match privilege {
        Privilege::Root => Ok(()),
        Privilege::NonRoot => {
            if lexically_normalize(candidate).starts_with(lexically_normalize(home)) {
                Ok(())
            } else {
                Err(CoreError::CustomPathOutsideHome {
                    path: candidate.to_path_buf(),
                    home: home.to_path_buf(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Two synthetic Categories with a deliberately overlapping allowlist, ordered
    /// high-to-low precedence exactly like [`ALL`], so the precedence rule can be pinned
    /// down without depending on the real Categories' (currently non-overlapping) allowlist
    /// choices.
    const HIGH: Category = Category {
        name: "High",
        sensitive: true,
        smart_default: true,
        allowlist: &[".config/shared"],
        sensitive_entries: &[],
        always_shown: false,
    };
    const LOW: Category = Category {
        name: "Low",
        sensitive: false,
        smart_default: true,
        allowlist: &[".config/shared/nested", ".config/only-low"],
        sensitive_entries: &[],
        always_shown: false,
    };

    #[test]
    fn agents_category_individually_tags_mcp_auth_but_not_claude_or_agents_dirs() {
        // `.mcp-auth` carries a per-entry sensitivity tag distinct from both the Category
        // flag (AGENTS.sensitive is false — a whole-Category tag would be too broad, since
        // `.claude`/`.agents` aren't credential-bearing) and the Safety-net scan (CONTEXT.md's
        // third "Sensitive" origin) — pinned down here so a future reshuffle of AGENTS's
        // allowlist can't silently drop it.
        assert!(entry_is_individually_sensitive(
            &AGENTS,
            Path::new(".mcp-auth/session.json")
        ));
        assert!(!entry_is_individually_sensitive(
            &AGENTS,
            Path::new(".claude/settings.json")
        ));
        assert!(!entry_is_individually_sensitive(
            &AGENTS,
            Path::new(".agents/config.toml")
        ));
    }

    #[test]
    fn entry_is_individually_sensitive_is_false_with_no_tagged_entries() {
        assert!(!entry_is_individually_sensitive(
            &LOW,
            Path::new(".config/only-low/file.txt")
        ));
    }

    #[test]
    fn resolve_category_prefers_higher_precedence_on_overlap() {
        let categories = [HIGH, LOW];
        let overlapping = PathBuf::from(".config/shared/nested/file.txt");

        // Both HIGH (".config/shared") and LOW (".config/shared/nested") match this path;
        // HIGH must win purely because it comes first in `categories`, regardless of which
        // allowlist entry is the more specific prefix.
        let (index, resolved) =
            resolve_category(&overlapping, &categories).expect("should resolve to a category");
        assert_eq!(resolved.name, "High");
        assert_eq!(index, 0);
    }

    #[test]
    fn resolve_category_falls_back_to_only_matching_category() {
        let categories = [HIGH, LOW];
        let low_only = PathBuf::from(".config/only-low/file.txt");

        let (index, resolved) =
            resolve_category(&low_only, &categories).expect("should resolve to a category");
        assert_eq!(resolved.name, "Low");
        assert_eq!(index, 1);
    }

    #[test]
    fn resolve_category_returns_none_when_no_allowlist_matches() {
        let categories = [HIGH, LOW];
        let uncovered = PathBuf::from("some/unrelated/path.txt");

        // Story 8: a path outside every allowlist resolves to nothing here — it's only
        // addable via Custom paths, never silently attributed to one of the ten.
        assert!(resolve_category(&uncovered, &categories).is_none());
    }

    #[test]
    fn all_ten_categories_are_defined_in_documented_precedence_order() {
        let names: Vec<&str> = ALL.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "SSH",
                "GPG",
                "Cloud creds",
                "KDE Plasma",
                "Agents",
                "Shell",
                "Program configs",
                "Package lists",
                "User data",
                "Custom paths",
            ]
        );
    }

    #[test]
    fn sensitive_categories_default_on_user_data_and_custom_paths_default_off() {
        // Looked up by name through `ALL` (rather than referencing e.g. `SSH.smart_default`
        // directly) so this pins down the *values in the taxonomy*, not a fact clippy can
        // fold away at compile time.
        let by_name = |name: &str| ALL.iter().find(|c| c.name == name).unwrap();

        for sensitive_category in ["SSH", "GPG", "Cloud creds"] {
            let category = by_name(sensitive_category);
            assert!(
                category.smart_default && category.sensitive,
                "{sensitive_category} must default ON and be Sensitive"
            );
        }
        assert!(
            !by_name("User data").smart_default,
            "User data must default OFF (story 4)"
        );
        assert!(
            !by_name("Custom paths").smart_default,
            "Custom paths starts empty, nothing to default on"
        );
    }

    #[test]
    fn only_custom_paths_is_exempt_from_hide_empty() {
        let always_shown: Vec<&str> = ALL
            .iter()
            .filter(|c| c.always_shown)
            .map(|c| c.name)
            .collect();
        assert_eq!(always_shown, vec!["Custom paths"]);
    }

    #[test]
    fn root_may_add_any_custom_path() {
        let home = PathBuf::from("/home/alice");
        let outside = PathBuf::from("/etc/nixos/configuration.nix");
        assert!(validate_custom_path(&outside, &home, Privilege::Root).is_ok());
    }

    #[test]
    fn non_root_custom_path_must_be_under_home() {
        let home = PathBuf::from("/home/alice");
        let inside = home.join("notes/todo.txt");
        let outside = PathBuf::from("/etc/nixos/configuration.nix");

        assert!(validate_custom_path(&inside, &home, Privilege::NonRoot).is_ok());
        assert!(matches!(
            validate_custom_path(&outside, &home, Privilege::NonRoot),
            Err(CoreError::CustomPathOutsideHome { .. })
        ));
    }

    #[test]
    fn non_root_traversal_escape_is_rejected() {
        let home = PathBuf::from("/home/alice");

        // Each of these lexically "starts with" $HOME before its `..` components walk back
        // out — the escape #44 closes. All must resolve outside $HOME and be rejected.
        for escape in [
            "/home/alice/../etc/passwd",
            "/home/alice/../../etc/x",
            "/home/alice/./../x",
            "/home/alice/subdir/../../secret",
            "/home/alice/..",
        ] {
            let candidate = PathBuf::from(escape);
            assert!(
                matches!(
                    validate_custom_path(&candidate, &home, Privilege::NonRoot),
                    Err(CoreError::CustomPathOutsideHome { .. })
                ),
                "expected {escape} to be rejected as a $HOME escape"
            );
        }
    }

    #[test]
    fn non_root_dot_components_that_stay_under_home_are_accepted() {
        let home = PathBuf::from("/home/alice");

        // `.`/`..` are fine as long as the normalized path stays under $HOME.
        for legit in [
            "/home/alice/notes/./todo.txt",
            "/home/alice/a/b/c",
            "/home/alice/a/../b",
            "/home/alice/./config",
            "/home/alice",
        ] {
            let candidate = PathBuf::from(legit);
            assert!(
                validate_custom_path(&candidate, &home, Privilege::NonRoot).is_ok(),
                "expected {legit} to be accepted as under $HOME"
            );
        }
    }

    #[test]
    fn root_traversal_escape_is_still_allowed() {
        let home = PathBuf::from("/home/alice");
        let escape = PathBuf::from("/home/alice/../../etc/x");
        assert!(validate_custom_path(&escape, &home, Privilege::Root).is_ok());
    }

    #[test]
    fn non_root_sibling_prefix_is_rejected() {
        let home = PathBuf::from("/home/alice");

        // A sibling whose name merely shares a string prefix with $HOME must not be mistaken
        // for a path under it — `starts_with` is component-wise, and this locks that in.
        for sibling in ["/home/alicia", "/home/alice-evil", "/home/alice2/secret"] {
            let candidate = PathBuf::from(sibling);
            assert!(
                matches!(
                    validate_custom_path(&candidate, &home, Privilege::NonRoot),
                    Err(CoreError::CustomPathOutsideHome { .. })
                ),
                "expected {sibling} to be rejected as a $HOME sibling, not under it"
            );
        }
    }
}
