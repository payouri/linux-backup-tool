//! `walkdir`-based discovery of a Category's Entries on the current machine: Hide-empty
//! computation (a Category with no matching paths is hidden from Selection), sizing, and
//! the Safety-net scan for credential-like filenames/extensions outside declared-sensitive
//! Categories.
//!
//! #19 scope: just the walkdir discovery of a single Category's Curated allowlist under
//! `RunContext.home`, producing [`model::Entry`] values. Hide-empty and the Safety-net
//! scan are #20/#22's job.

use std::path::Path;

use walkdir::WalkDir;

use crate::catalog::Category;
use crate::context::RunContext;
use crate::model::{Entry, EntryRoot};

/// Walks `category`'s Curated allowlist under `ctx.home`, producing one [`Entry`] per
/// regular file found (directories on the allowlist are walked recursively; missing
/// allowlist roots are silently skipped — that's Hide-empty's job at the Category level,
/// not an error at the file level).
///
/// Every Entry produced here is `EntryRoot::Home` and inherits `category.sensitive`,
/// since #19 only needs one Category to drive the round-trip seam.
pub fn scan_category(category: &Category, ctx: &RunContext) -> Vec<Entry> {
    let mut entries = Vec::new();

    for allowlist_root in category.allowlist {
        let root_path = ctx.home.join(allowlist_root);
        if !root_path.exists() {
            continue;
        }

        for walked in WalkDir::new(&root_path).into_iter().filter_map(Result::ok) {
            if !walked.file_type().is_file() {
                continue;
            }
            let absolute_path = walked.path().to_path_buf();
            let relative_path = relative_to(&absolute_path, &ctx.home);
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

/// Strips `base` off `path`, panicking-free: falls back to `path` itself if `path` isn't
/// actually under `base` (shouldn't happen for paths this module produces itself).
fn relative_to(path: &Path, base: &Path) -> std::path::PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}
