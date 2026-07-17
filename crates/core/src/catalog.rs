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

use std::path::Path;

use crate::context::Privilege;
use crate::error::CoreError;

/// A named group a path can belong to, backed by a Curated allowlist. See CONTEXT.md's
/// "Category" entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    /// The Category's name, used verbatim in the Manifest's `entries[].category` field.
    pub name: &'static str,
    /// Whether every Entry in this Category is Sensitive (a "sensitive Category" origin,
    /// per CONTEXT.md's "Sensitive" entry). This is the Category-level tag slot #22's
    /// per-entry tagging and Safety-net scan build on top of — it is not itself that
    /// finer-grained tagging.
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
    always_shown: false,
};

/// GPG: the whole `~/.gnupg`. Sensitive, default-ON (story 3).
pub const GPG: Category = Category {
    name: "GPG",
    sensitive: true,
    smart_default: true,
    allowlist: &[".gnupg"],
    always_shown: false,
};

/// Cloud creds: the credential directories of the common CLI-driven cloud providers.
/// Sensitive, default-ON (story 3).
pub const CLOUD_CREDS: Category = Category {
    name: "Cloud creds",
    sensitive: true,
    smart_default: true,
    allowlist: &[".aws", ".config/gcloud", ".azure"],
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
    always_shown: false,
};

/// Agents: AI-agent tool state (`.claude`/`.agents`/`.mcp-auth`). Some Entries are
/// individually sensitivity-tagged (e.g. auth files under `.mcp-auth`) — that finer-grained
/// tagging is #22's job; the Category itself is not blanket-Sensitive.
pub const AGENTS: Category = Category {
    name: "Agents",
    sensitive: false,
    smart_default: true,
    allowlist: &[".claude", ".agents", ".mcp-auth"],
    always_shown: false,
};

/// Shell: `oh-my-zsh/custom/` only, never the whole oh-my-zsh framework (story: "Shell
/// (`oh-my-zsh/custom/` only, not the whole framework)"). Unchanged from #19.
pub const SHELL: Category = Category {
    name: "Shell",
    sensitive: false,
    smart_default: true,
    allowlist: &[".oh-my-zsh/custom"],
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

/// Resolves which Category owns `relative_path` when it may match more than one entry in
/// `categories`: the first (i.e. highest-precedence) Category in `categories` whose
/// allowlist contains it. Callers pass Categories already in precedence order (e.g.
/// [`ALL`]) — this function does not re-sort or otherwise interpret ordering, it just picks
/// the first match, which is what makes "higher-precedence wins" a one-line rule instead of
/// a comparison function.
///
/// Returns `None` if `relative_path` matches no Category's allowlist at all (story 8: such
/// a path is only addable via Custom paths, never silently attributed to one of the ten).
pub fn resolve_category<'a>(
    relative_path: &Path,
    categories: &'a [Category],
) -> Option<&'a Category> {
    categories
        .iter()
        .find(|category| category_matches(category, relative_path))
}

/// Validates a Custom path against the root/non-root constraint (stories 10/11): root may
/// add any filesystem path; non-root is constrained to under `home`. `privilege` is always
/// the injected [`Privilege`] (ADR-0004) — this function never reads the real euid itself.
pub fn validate_custom_path(
    candidate: &Path,
    home: &Path,
    privilege: Privilege,
) -> Result<(), CoreError> {
    match privilege {
        Privilege::Root => Ok(()),
        Privilege::NonRoot => {
            if candidate.starts_with(home) {
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
        always_shown: false,
    };
    const LOW: Category = Category {
        name: "Low",
        sensitive: false,
        smart_default: true,
        allowlist: &[".config/shared/nested", ".config/only-low"],
        always_shown: false,
    };

    #[test]
    fn resolve_category_prefers_higher_precedence_on_overlap() {
        let categories = [HIGH, LOW];
        let overlapping = PathBuf::from(".config/shared/nested/file.txt");

        // Both HIGH (".config/shared") and LOW (".config/shared/nested") match this path;
        // HIGH must win purely because it comes first in `categories`, regardless of which
        // allowlist entry is the more specific prefix.
        let resolved =
            resolve_category(&overlapping, &categories).expect("should resolve to a category");
        assert_eq!(resolved.name, "High");
    }

    #[test]
    fn resolve_category_falls_back_to_only_matching_category() {
        let categories = [HIGH, LOW];
        let low_only = PathBuf::from(".config/only-low/file.txt");

        let resolved =
            resolve_category(&low_only, &categories).expect("should resolve to a category");
        assert_eq!(resolved.name, "Low");
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
}
