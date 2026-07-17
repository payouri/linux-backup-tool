//! `walkdir`-based discovery of Categories' Entries on the current machine: precedence
//! resolution across overlapping allowlists, Hide-empty computation (a Category with no
//! matching paths is hidden from Selection), and sizing. The Safety-net scan for
//! credential-like filenames/extensions outside declared-sensitive Categories is #22's job.

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
            entries.push(Entry {
                absolute_path,
                relative_path,
                root: EntryRoot::Home,
                category: category.name,
                sensitive: category.sensitive,
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
        let owner_index = categories.iter().position(|c| c.name == owner.name).expect("owner came from categories");
        entries_by_category[owner_index].push(Entry {
            absolute_path,
            relative_path,
            root: EntryRoot::Home,
            category: owner.name,
            sensitive: owner.sensitive,
        });
    }

    categories
        .into_iter()
        .zip(entries_by_category)
        .filter(|(category, entries)| !entries.is_empty() || category.always_shown)
        .map(|(category, entries)| CategoryScan { category, entries })
        .collect()
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
        fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n").expect("write aliases.zsh");

        let ctx = test_ctx(home);
        let scanned = scan_taxonomy(&ctx);

        let names: Vec<&str> = scanned.iter().map(|s| s.category.name).collect();
        assert!(names.contains(&"Shell"), "Shell has a match and must be shown");
        assert!(names.contains(&"Custom paths"), "Custom paths is exempt from Hide-empty");
        assert!(!names.contains(&"SSH"), "SSH has zero matches on this fixture and must be hidden");
        assert!(!names.contains(&"GPG"), "GPG has zero matches on this fixture and must be hidden");
        assert!(
            !names.contains(&"Package lists"),
            "Package lists has no filesystem allowlist and is never populated here"
        );

        let custom_paths_scan = scanned.iter().find(|s| s.category.name == "Custom paths").unwrap();
        assert!(custom_paths_scan.entries.is_empty(), "Custom paths starts empty (story 6)");
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
        fs::write(custom_dir.join("aliases.zsh"), b"alias ll='ls -la'\n").expect("write aliases.zsh");

        let ctx = test_ctx(home);
        let scanned = scan_taxonomy(&ctx);

        let ssh_scan = scanned.iter().find(|s| s.category.name == "SSH").expect("SSH should be present");
        assert_eq!(ssh_scan.entries.len(), 1);
        assert!(ssh_scan.entries[0].sensitive, "SSH is a sensitive Category");

        let shell_scan = scanned.iter().find(|s| s.category.name == "Shell").expect("Shell should be present");
        assert_eq!(shell_scan.entries.len(), 1);
        assert!(!shell_scan.entries[0].sensitive, "Shell is not a sensitive Category");
    }
}
