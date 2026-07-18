//! `walkdir`-based discovery of Categories' Entries on the current machine: precedence
//! resolution across overlapping allowlists, Hide-empty computation (a Category with no
//! matching paths is hidden from Selection), sizing, and the two per-Entry sensitivity
//! origins beyond a Category's own blanket tag: `catalog::entry_is_individually_sensitive`
//! (a declared per-entry tag, e.g. Agents' `.mcp-auth`) and the Safety-net scan for
//! credential-like filenames/extensions (CONTEXT.md's "Safety-net scan", story 16). All
//! three origins OR together per CONTEXT.md's "Sensitive" entry.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::catalog::{self, Category};
use crate::context::RunContext;
use crate::model::{Entry, EntryRoot};

/// Walks `category`'s Curated allowlist under `ctx.home`, producing one [`Entry`] per
/// regular file found (directories on the allowlist are walked recursively; missing
/// allowlist roots are silently skipped).
///
/// This scans a single Category in isolation, with no awareness of any other Category's
/// allowlist — so it does **not** apply precedence resolution (see [`scan_taxonomy`] for
/// that). It stays useful on its own for the common case of "just this one Category,"
/// which is all #19's round-trip seam needed and still all a single-Category caller needs.
pub fn scan_category(category: &Category, ctx: &RunContext) -> Vec<Entry> {
    let mut entries = Vec::new();

    for allowlist_root in category.allowlist {
        for (absolute_path, relative_path) in walk_allowlist_root(&ctx.home, allowlist_root) {
            // Strict superset (CONTEXT.md's "Safety-net scan"): each origin can only *add*
            // sensitivity, never remove what another origin already set.
            let sensitive = category.sensitive
                || catalog::entry_is_individually_sensitive(category, &relative_path)
                || is_safety_net_match(&relative_path);
            entries.push(Entry {
                absolute_path,
                relative_path,
                root: EntryRoot::Home,
                category: category.name,
                sensitive,
            });
        }
    }

    entries
}

/// One Category's realized state on this machine: the Category itself and the Entries
/// that resolved to it after precedence (see [`scan_taxonomy`]).
#[derive(Debug, Clone)]
pub struct CategoryScan {
    pub category: Category,
    pub entries: Vec<Entry>,
}

impl CategoryScan {
    /// The sum of every one of this Category's discovered Entries' current on-disk size in
    /// bytes — independent of whether any of them are actually part of the active Selection
    /// yet (contrast `SelectionModel::total_size`, which only sums what's actually
    /// selected). This is the per-Category size the WHAT screen (#30) shows next to a
    /// Category's checkbox, so a user can weigh a Category before deciding to turn it on.
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|e| std::fs::metadata(&e.absolute_path).ok())
            .map(|m| m.len())
            .sum()
    }

    /// Whether this Category should carry the WHAT screen's 🔒 annotation (#30): true if
    /// the Category itself is blanket-Sensitive, or if any of its discovered Entries
    /// individually is (a per-entry tag or Safety-net match, both already resolved into
    /// `Entry::sensitive` by `scan_category`/`scan_taxonomy`) — so a Category that isn't
    /// itself Sensitive but happens to contain a flagged Entry still gets called out.
    pub fn is_sensitive(&self) -> bool {
        self.category.sensitive || self.entries.iter().any(|e| e.sensitive)
    }
}

/// Scans the full ten-Category taxonomy under `ctx.home`, resolving precedence across any
/// overlapping allowlists and applying Hide-empty.
///
/// A file can surface while walking more than one Category's allowlist only when one
/// Category's root is a prefix of another's (e.g. a broad catch-all root nesting a more
/// specific one) — real overlap in the current allowlists is not expected, but the
/// precedence rule (CONTEXT.md's Category precedence) exists precisely to make that
/// unambiguous rather than accidental. So candidates are gathered from every Category's
/// allowlist, deduplicated by absolute path, and each is attributed to exactly one
/// Category — the highest-precedence one whose allowlist contains it
/// (`catalog::resolve_category`) — before any Category's entry list is finalized.
///
/// Hide-empty (story 5) then drops any Category with zero resolved Entries, except a
/// Category marked `always_shown` (only Custom paths, story 6), which is kept regardless so
/// the escape hatch is never itself hidden.
///
/// Package lists never produces Entries here (its allowlist is empty by design — see
/// `catalog::PACKAGE_LISTS`'s doc) and so is Hidden by this function; its own content
/// reaches the Manifest through `packages::capture_all` instead.
pub fn scan_taxonomy(ctx: &RunContext) -> Vec<CategoryScan> {
    let categories = catalog::ALL;

    // Dedup by absolute path: without this, a file under two Categories' allowlist roots
    // (the overlap case above) would be walked twice and, worse, would let precedence
    // resolution run once per walk instead of once per file.
    let mut seen_paths = HashSet::new();
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new(); // (absolute, relative-to-home)
    for category in &categories {
        for allowlist_root in category.allowlist {
            for (absolute_path, relative_path) in walk_allowlist_root(&ctx.home, allowlist_root) {
                if seen_paths.insert(absolute_path.clone()) {
                    candidates.push((absolute_path, relative_path));
                }
            }
        }
    }

    let mut entries_by_category: Vec<Vec<Entry>> = vec![Vec::new(); categories.len()];
    for (absolute_path, relative_path) in candidates {
        // `resolve_category` already returns the highest-precedence match across the whole
        // (precedence-ordered) `categories` slice, so a path never needs to be checked
        // Category-by-Category here — one call settles ownership.
        let Some(owner) = catalog::resolve_category(&relative_path, &categories) else {
            // No allowlist covers this path — story 8: only addable via Custom paths, never
            // silently attributed to one of the ten here.
            continue;
        };
        // ADR-0002: backup fails fast on bugs rather than silently dropping data; `owner`
        // came from `categories` above, so this can only panic on a real invariant break.
        #[allow(clippy::disallowed_methods)]
        let owner_index = categories
            .iter()
            .position(|c| c.name == owner.name)
            .expect("owner came from categories");
        // Strict superset, same as `scan_category`: each origin can only add sensitivity on
        // top of what another already set, never remove it.
        let sensitive = owner.sensitive
            || catalog::entry_is_individually_sensitive(owner, &relative_path)
            || is_safety_net_match(&relative_path);
        entries_by_category[owner_index].push(Entry {
            absolute_path,
            relative_path,
            root: EntryRoot::Home,
            category: owner.name,
            sensitive,
        });
    }

    categories
        .into_iter()
        .zip(entries_by_category)
        .filter(|(category, entries)| !entries.is_empty() || category.always_shown)
        .map(|(category, entries)| CategoryScan { category, entries })
        .collect()
}

/// Credential-like keyword substrings the Safety-net scan looks for in a filename,
/// case-insensitively. Verbatim list from CONTEXT.md's "Safety-net scan" entry / issue #22.
const SAFETY_NET_KEYWORDS: &[&str] = &["credential", "secret", "token", "password"];

/// Credential-like file extensions the Safety-net scan flags outright, regardless of the
/// rest of the filename.
const SAFETY_NET_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx"];

/// Exact-prefix credential filenames the Safety-net scan flags. An SSH private key carries
/// no extension, so this checks the name itself rather than a suffix; `id_rsa.pub` also
/// starts with `id_rsa` and is deliberately swept in too rather than special-cased out of
/// the match, since the scan is a strict superset by design (never a precise ID of exactly
/// which sibling file is the private half).
const SAFETY_NET_NAME_PREFIXES: &[&str] = &["id_rsa", "id_ed25519"];

/// Whether `path`'s filename looks credential-like enough to flag Sensitive on its own,
/// independent of which Category (if any) it resolved to (CONTEXT.md's "Safety-net scan",
/// story 16). Pure function over a path — no filesystem access, no Category awareness — so
/// it composes the same way whether called from `scan_category` or `scan_taxonomy`.
pub fn is_safety_net_match(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = file_name.to_lowercase();

    // `.netrc` has no distinguishing extension or keyword substring of its own, so it's
    // checked as an exact name rather than folded into the prefix/extension lists above.
    if lower == ".netrc" {
        return true;
    }
    if SAFETY_NET_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    if SAFETY_NET_NAME_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    SAFETY_NET_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Walks one allowlist root (relative to `home`) into `(absolute_path, relative_path)`
/// pairs for every regular file found. A missing root is silently skipped — that's
/// Hide-empty's job at the Category level, not an error at the file level.
fn walk_allowlist_root(home: &Path, allowlist_root: &str) -> Vec<(PathBuf, PathBuf)> {
    let root_path = home.join(allowlist_root);
    if !root_path.exists() {
        return Vec::new();
    }

    WalkDir::new(&root_path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|walked| walked.file_type().is_file())
        .map(|walked| {
            let absolute_path = walked.path().to_path_buf();
            let relative_path = relative_to(&absolute_path, home);
            (absolute_path, relative_path)
        })
        .collect()
}

/// Strips `base` off `path`, panicking-free: falls back to `path` itself if `path` isn't
/// actually under `base` (shouldn't happen for paths this module produces itself).
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Privilege;
    use std::fs;
    use std::time::UNIX_EPOCH;

    fn test_ctx(home: PathBuf) -> RunContext {
        RunContext {
            home,
            output_location: PathBuf::from("/tmp/unused"),
            privilege: Privilege::NonRoot,
            host: "test-host".to_string(),
            user: "test-user".to_string(),
            uid: 1000,
            os: "linux-test".to_string(),
            now: UNIX_EPOCH,
        }
    }

    #[test]
    fn hide_empty_drops_categories_with_no_matches_but_keeps_custom_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        // Only Shell has a matching file; every other Category's allowlist roots are
        // absent from this fixture $HOME.
        let custom_dir = home.join(".oh-my-zsh/custom");
        fs::create_dir_all(&custom_dir).expect("create custom dir");
        fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n")
            .expect("write aliases.zsh");

        let ctx = test_ctx(home);
        let scanned = scan_taxonomy(&ctx);

        let names: Vec<&str> = scanned.iter().map(|s| s.category.name).collect();
        assert!(
            names.contains(&"Shell"),
            "Shell has a match and must be shown"
        );
        assert!(
            names.contains(&"Custom paths"),
            "Custom paths is exempt from Hide-empty"
        );
        assert!(
            !names.contains(&"SSH"),
            "SSH has zero matches on this fixture and must be hidden"
        );
        assert!(
            !names.contains(&"GPG"),
            "GPG has zero matches on this fixture and must be hidden"
        );
        assert!(
            !names.contains(&"Package lists"),
            "Package lists has no filesystem allowlist and is never populated here"
        );

        let custom_paths_scan = scanned
            .iter()
            .find(|s| s.category.name == "Custom paths")
            .unwrap();
        assert!(
            custom_paths_scan.entries.is_empty(),
            "Custom paths starts empty (story 6)"
        );
    }

    #[test]
    fn taxonomy_scan_resolves_multiple_present_categories_independently() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");

        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("create ssh dir");
        fs::write(ssh_dir.join("id_ed25519"), b"fake-key\n").expect("write ssh key");

        let custom_dir = home.join(".oh-my-zsh/custom");
        fs::create_dir_all(&custom_dir).expect("create custom dir");
        fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n")
            .expect("write aliases.zsh");

        let ctx = test_ctx(home);
        let scanned = scan_taxonomy(&ctx);

        let ssh_scan = scanned
            .iter()
            .find(|s| s.category.name == "SSH")
            .expect("SSH should be present");
        assert_eq!(ssh_scan.entries.len(), 1);
        assert!(ssh_scan.entries[0].sensitive, "SSH is a sensitive Category");

        let shell_scan = scanned
            .iter()
            .find(|s| s.category.name == "Shell")
            .expect("Shell should be present");
        assert_eq!(shell_scan.entries.len(), 1);
        assert!(
            !shell_scan.entries[0].sensitive,
            "Shell is not a sensitive Category"
        );
    }

    #[test]
    fn safety_net_matches_documented_keyword_substrings_case_insensitively() {
        for name in [
            "my-secret.txt",
            "SECRET.txt",
            "credentials.json",
            "TOKEN",
            "password.env",
        ] {
            assert!(
                is_safety_net_match(Path::new(name)),
                "{name} should match a Safety-net keyword"
            );
        }
    }

    #[test]
    fn safety_net_matches_documented_extensions() {
        for name in ["site.pem", "server.KEY", "bundle.p12", "bundle.pfx"] {
            assert!(
                is_safety_net_match(Path::new(name)),
                "{name} should match a Safety-net extension"
            );
        }
    }

    #[test]
    fn safety_net_matches_ssh_key_name_prefixes_and_netrc() {
        for name in ["id_rsa", "id_rsa.pub", "id_ed25519", ".netrc"] {
            assert!(
                is_safety_net_match(Path::new(name)),
                "{name} should match a Safety-net name pattern"
            );
        }
    }

    #[test]
    fn safety_net_does_not_flag_unrelated_filenames() {
        for name in ["aliases.zsh", "report.pdf", "kwinrc", "not-included.txt"] {
            assert!(
                !is_safety_net_match(Path::new(name)),
                "{name} should not match any Safety-net pattern"
            );
        }
    }

    #[test]
    fn scan_category_flags_safety_net_match_even_in_a_non_sensitive_category() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let git_dir = home.join(".config/git");
        fs::create_dir_all(&git_dir).expect("create .config/git dir");
        // "credentials" contains the "credential" keyword; Program configs (the owning
        // Category here) is not itself Sensitive, so this only flags via Safety-net.
        fs::write(git_dir.join("credentials"), b"fake-token\n").expect("write credentials file");

        let ctx = test_ctx(home);
        let entries = scan_category(&catalog::PROGRAM_CONFIGS, &ctx);

        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].sensitive,
            "Safety-net match must flag the Entry Sensitive even though its Category isn't"
        );
    }

    #[test]
    fn scan_category_never_un_flags_a_sensitive_category_entry() {
        // Superset property: a Category-sensitive Entry stays sensitive whether or not its
        // filename also happens to match a Safety-net pattern.
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("create .ssh dir");
        fs::write(ssh_dir.join("config"), b"Host example.com\n").expect("write ssh config");

        let ctx = test_ctx(home);
        let entries = scan_category(&catalog::SSH, &ctx);

        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].sensitive,
            "SSH is a sensitive Category regardless of the filename ('config' matches no \
             Safety-net pattern)"
        );
    }

    #[test]
    fn scan_category_flags_a_per_entry_tagged_file_the_safety_net_scan_would_miss() {
        // "session.json" matches no Safety-net keyword/extension/prefix at all, and Agents
        // is not a blanket-Sensitive Category — the only way this Entry can end up
        // `sensitive: true` is the per-entry tag on `.mcp-auth`
        // (`catalog::entry_is_individually_sensitive`), proving that origin is real and not
        // just a rationalization that the Safety-net scan happens to also catch it.
        assert!(!is_safety_net_match(Path::new("session.json")));

        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let mcp_auth_dir = home.join(".mcp-auth");
        fs::create_dir_all(&mcp_auth_dir).expect("create .mcp-auth dir");
        fs::write(mcp_auth_dir.join("session.json"), b"{}\n").expect("write session.json");

        let ctx = test_ctx(home);
        let entries = scan_category(&catalog::AGENTS, &ctx);

        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].sensitive,
            "the per-entry tag on .mcp-auth must flag this Entry Sensitive even though \
             neither the Category nor the Safety-net scan would"
        );
    }

    #[test]
    fn category_scan_total_size_sums_every_discovered_entry_regardless_of_selection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ssh_dir = tmp.path().join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("create .ssh dir");
        fs::write(ssh_dir.join("id_ed25519"), vec![0u8; 100]).expect("write key");
        fs::write(ssh_dir.join("config"), vec![0u8; 50]).expect("write config");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let entries = scan_category(&catalog::SSH, &ctx);
        let scan = CategoryScan {
            category: catalog::SSH,
            entries,
        };

        assert_eq!(scan.total_size(), 150);
    }

    #[test]
    fn category_scan_is_sensitive_when_the_category_itself_is_sensitive() {
        let scan = CategoryScan {
            category: catalog::SSH,
            entries: Vec::new(),
        };
        assert!(scan.is_sensitive());
    }

    #[test]
    fn category_scan_is_sensitive_when_a_non_sensitive_category_has_a_flagged_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mcp_auth_dir = tmp.path().join(".mcp-auth");
        fs::create_dir_all(&mcp_auth_dir).expect("create .mcp-auth dir");
        fs::write(mcp_auth_dir.join("session.json"), b"{}\n").expect("write session.json");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let entries = scan_category(&catalog::AGENTS, &ctx);
        let scan = CategoryScan {
            category: catalog::AGENTS,
            entries,
        };

        assert!(
            scan.is_sensitive(),
            "AGENTS itself isn't Sensitive, but its .mcp-auth Entry is"
        );
    }

    #[test]
    fn category_scan_is_not_sensitive_with_a_plain_category_and_no_flagged_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom_dir = tmp.path().join(".oh-my-zsh/custom");
        fs::create_dir_all(&custom_dir).expect("create custom dir");
        fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n").expect("write aliases");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let entries = scan_category(&catalog::SHELL, &ctx);
        let scan = CategoryScan {
            category: catalog::SHELL,
            entries,
        };

        assert!(!scan.is_sensitive());
    }
}
