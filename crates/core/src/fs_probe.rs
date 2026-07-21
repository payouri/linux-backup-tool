//! Filesystem *content* probes at `core`'s boundary (ADR-0005 D6).
//!
//! These are the reads a `tui` (or future `gui`) view would otherwise do itself: a
//! subtree's on-disk size, a directory-existence check, the encrypted-Backup signal, and a
//! directory listing for path autocomplete. ADR-0005 D6 pulls all filesystem *content* I/O
//! into `core` so a view renders what `core` returns rather than touching the disk directly.
//!
//! They are deliberately plain functions over an explicit path — no trait (there is a single
//! consumer for each, so ADR-0001's no-abstraction-until-a-second-impl reasoning applies) and
//! no injected [`RunContext`](crate::RunContext): none needs an ambient run fact. The one
//! carve-out stays at the `tui` boundary — env-var expansion of a user-typed path is resolved
//! by `tui`'s resolver closure (ADR-0004), never read here; these functions receive a path
//! that is already expanded.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::archive;
use crate::error::CoreError;

/// Total on-disk byte size of `paths`, summing each path's own `metadata` length and
/// skipping any that can't be read.
///
/// Backs the Drill-down root row's subtree size (#30): a heavy allowlist root stands for its
/// already-expanded Entries, so the caller passes those Entries' absolute paths and this sums
/// them. Skipping unreadable paths mirrors [`SelectionModel::total_size`](crate::SelectionModel::total_size) —
/// a display size never fails a screen over one stat error.
pub fn subtree_size<'a>(paths: impl IntoIterator<Item = &'a Path>) -> u64 {
    paths
        .into_iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Whether `path` is an existing, reachable directory — the existence check the `restore`
/// command runs against its Backup-dir argument before doing anything else.
pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// Whether `backup_dir` holds an encrypted Backup, detected the same way `restore` does
/// internally: the `.age`-suffixed Manifest filename is the authoritative signal (CONTEXT.md's
/// Manifest entry — never a field inside the Manifest).
///
/// Checking it ahead of `restore::restore` is what lets a caller prompt for the passphrase up
/// front (story 50) rather than discovering encryption only once restore is already underway.
pub fn is_encrypted_backup(backup_dir: &Path) -> bool {
    backup_dir
        .join(archive::filename_for(archive::MANIFEST_FILENAME, true))
        .is_file()
}

/// One entry in a [`list_dir`] listing for path autocomplete (#52).
///
/// `is_dir` follows symlinks, so a symlink pointing at a directory reads as descendable, while
/// `is_symlink` records that the entry itself is a link so a view can mark it (symlinks are
/// marked and descendable; the non-root `$HOME` scope check stays lexical at the `tui`
/// boundary, matching `catalog::validate_custom_path` — symlink-escape is a documented
/// limitation, #52).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: OsString,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// List the immediate children of `dir` for path autocomplete (#52).
///
/// A directory the current user can't read yields an empty listing rather than an error — the
/// privilege-aware "no candidates" case autocomplete wants — so completion degrades quietly
/// when a typed path crosses into an unreadable directory. Per-entry stat failures are skipped
/// for the same reason.
///
/// Ordering, dotfile hiding, and the non-root `$HOME` scope are the caller's
/// presentation/policy concerns, not this probe's.
///
/// # Errors
///
/// Returns [`CoreError::Io`] if `dir` can't be read for a reason other than permission denied
/// (e.g. it doesn't exist or isn't a directory).
pub fn list_dir(dir: &Path) -> Result<Vec<DirEntry>, CoreError> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(source) if source.kind() == ErrorKind::PermissionDenied => return Ok(Vec::new()),
        Err(source) => {
            return Err(CoreError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut entries = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        // `file_type` from `read_dir` does not follow the link, so it is how we learn the
        // entry *is* a symlink; `metadata` follows it, so it is what tells us the link target
        // is a directory (and thus descendable).
        let is_symlink = entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_symlink());
        let is_dir = fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
        entries.push(DirEntry {
            name: entry.file_name(),
            is_dir,
            is_symlink,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn subtree_size_sums_readable_paths_and_skips_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::File::create(&a)
            .expect("create a")
            .set_len(100)
            .expect("set_len a");
        fs::File::create(&b)
            .expect("create b")
            .set_len(200)
            .expect("set_len b");
        let missing = tmp.path().join("missing");

        let paths = [a.as_path(), b.as_path(), missing.as_path()];
        assert_eq!(subtree_size(paths), 300);
    }

    #[test]
    fn subtree_size_of_nothing_is_zero() {
        let paths: [&Path; 0] = [];
        assert_eq!(subtree_size(paths), 0);
    }

    #[test]
    fn is_dir_is_true_for_a_directory_and_false_otherwise() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("file");
        fs::write(&file, b"x").expect("write");

        assert!(is_dir(tmp.path()));
        assert!(!is_dir(&file));
        assert!(!is_dir(&tmp.path().join("does-not-exist")));
    }

    #[test]
    fn is_encrypted_backup_is_true_when_the_age_suffixed_manifest_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("manifest.json.age"), b"ignored").expect("write");
        assert!(is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn is_encrypted_backup_is_false_when_only_the_plain_manifest_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("manifest.json"), b"ignored").expect("write");
        assert!(!is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn is_encrypted_backup_is_false_when_the_dir_has_neither_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!is_encrypted_backup(tmp.path()));
    }

    #[test]
    fn list_dir_reports_files_and_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("sub")).expect("create sub");
        fs::write(tmp.path().join("file.txt"), b"x").expect("write file");

        let mut listing = list_dir(tmp.path()).expect("list_dir");
        listing.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(listing.len(), 2);
        assert_eq!(listing[0].name, OsString::from("file.txt"));
        assert!(!listing[0].is_dir);
        assert!(!listing[0].is_symlink);
        assert_eq!(listing[1].name, OsString::from("sub"));
        assert!(listing[1].is_dir);
        assert!(!listing[1].is_symlink);
    }

    #[test]
    fn list_dir_of_an_empty_directory_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(list_dir(tmp.path()).expect("list_dir").is_empty());
    }

    #[test]
    fn list_dir_marks_a_symlink_to_a_directory_as_symlink_and_descendable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("real-dir");
        fs::create_dir(&target).expect("create target");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let listing = list_dir(tmp.path()).expect("list_dir");
        let entry = listing
            .iter()
            .find(|e| e.name.to_str() == Some("link"))
            .expect("link entry");
        assert!(entry.is_symlink);
        // Follows the link: the target is a directory, so autocomplete can descend into it.
        assert!(entry.is_dir);
    }

    #[test]
    fn list_dir_errors_on_a_nonexistent_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist");
        let err = list_dir(&missing).unwrap_err();
        assert!(matches!(err, CoreError::Io { path, .. } if path == missing));
    }

    #[test]
    fn list_dir_yields_no_candidates_for_an_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses permission bits, which would make this assertion flaky under
        // CI-as-root; skip there (the same pattern `location`'s write test documents). This
        // reads the test process's real euid, which is test infrastructure — not core
        // production logic — so it doesn't reopen ADR-0004's ban on core reading ambient facts.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).expect("create locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000");

        let listing = list_dir(&locked);

        // Restore permissions so tempdir cleanup can remove it.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("chmod 700");
        assert_eq!(
            listing.expect("unreadable dir is not an error"),
            Vec::<DirEntry>::new()
        );
    }

    #[test]
    fn list_dir_result_carries_the_missing_path() {
        // A regression guard for the `PathBuf` round-trip in the `Io` arm.
        let missing = PathBuf::from("/definitely/not/here/xyzzy");
        let err = list_dir(&missing).unwrap_err();
        assert!(matches!(err, CoreError::Io { path, .. } if path == missing));
    }
}
