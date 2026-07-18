//! Load/save/prune of `<config>/linux-backup/profile.toml` (`serde` + `toml`): the last
//! Selection (selected Category names, Custom paths, Drill-down deselected relative paths)
//! and last Output location (CONTEXT.md's "Profile").
//!
//! Per ADR-0004 `core` reads nothing ambient: the profile file's *path* is resolved in
//! `crates/tui` from `$XDG_CONFIG_HOME`/`$HOME` and handed in here, and prune's
//! filesystem-existence check arrives as an injected `exists` closure rather than a direct
//! disk read — so [`prune`] and the whole Profile↔[`SelectionModel`] mapping are unit-testable
//! against fake data with no real disk or environment.
//!
//! Stale paths are pruned on load (drop-with-note, never error — story 59). Encryption state
//! is deliberately never a field here (story 24): Auto-insist is recomputed fresh from the
//! Selection every run and a passphrase is never written anywhere.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog;
use crate::error::CoreError;
use crate::model::{Entry, EntryRoot, SelectionModel};
use crate::scan::{self, CategoryScan};

/// The persistable shape of a Selection plus its Output location — everything a returning run
/// pre-fills from (story 58). Deliberately holds no encryption state (story 24).
///
/// `selected_categories` and `custom_paths` are stored separately from the reconstructed
/// `SelectionModel` because a Category's Entries are re-discovered fresh by
/// `scan::scan_taxonomy` on the next run (files may have come or gone) — the Profile only has
/// to remember *which Categories were on*, not their Entry lists, which would go stale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// The names of every Category that was selected (had at least one active Entry).
    #[serde(default)]
    pub selected_categories: Vec<String>,
    /// The absolute paths of the Custom-paths Category's Entries — the one path-typed part of
    /// a Selection that can't be re-derived from a fresh scan, so it's stored verbatim and
    /// pruned on load if it no longer exists.
    #[serde(default)]
    pub custom_paths: Vec<PathBuf>,
    /// Drill-down deselected relative paths, straight from
    /// [`SelectionModel::deselected_paths`] (CONTEXT.md's "Drill-down": deselections persist).
    #[serde(default)]
    pub deselected_paths: Vec<PathBuf>,
    /// The last Output location (a directory, never the Backup dir itself). `None` after a
    /// prune drops it (the drive it named is gone) or when no run has bound one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_location: Option<PathBuf>,
}

/// A loaded Profile plus how many stale path-typed entries were dropped in the process — the
/// count the returning-run banner's "N pruned" line reports (story 60). `pruned` is 0 for a
/// Profile whose every remembered path still exists.
#[derive(Debug, Clone)]
pub struct LoadedProfile {
    pub profile: Profile,
    pub pruned: usize,
}

impl Profile {
    /// Captures the persistable state of `selection` plus the run's bound `output_location`.
    ///
    /// A Category counts as selected when it has at least one *active* Entry; the Custom-paths
    /// Category's active Entries are additionally recorded by absolute path (so they survive a
    /// scan that can't re-discover them). Category names are deduplicated and sorted so the
    /// written `profile.toml` is stable run-to-run when the Selection hasn't changed.
    pub fn from_selection(selection: &SelectionModel, output_location: Option<PathBuf>) -> Self {
        let selected_categories: Vec<String> = selection
            .entries()
            .iter()
            .map(|entry| entry.category.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let custom_paths = selection
            .entries()
            .iter()
            .filter(|entry| entry.category == catalog::CUSTOM_PATHS.name)
            .map(|entry| entry.absolute_path.clone())
            .collect();

        let deselected_paths = selection.deselected_paths().iter().cloned().collect();

        Self {
            selected_categories,
            custom_paths,
            deselected_paths,
            output_location,
        }
    }

    /// Reconstructs a [`SelectionModel`] to seed the WHAT screen with (story 58), against a
    /// *fresh* `scanned` taxonomy: every selected Category's freshly-discovered Entries, plus
    /// the remembered Custom paths as their own Entries, with the remembered Drill-down
    /// deselections re-applied on top.
    ///
    /// A remembered deselection whose Entry no longer scans is simply a no-op
    /// ([`SelectionModel::deselect_entry`] ignores an unknown path) — a vanished exclusion
    /// self-corrects (there's nothing left to exclude) rather than needing its own prune pass.
    pub fn to_selection_model(&self, scanned: &[CategoryScan], home: &Path) -> SelectionModel {
        let selected: BTreeSet<&str> = self
            .selected_categories
            .iter()
            .map(String::as_str)
            .collect();

        let mut entries: Vec<Entry> = scanned
            .iter()
            .filter(|scan| selected.contains(scan.category.name))
            .flat_map(|scan| scan.entries.iter().cloned())
            .collect();
        entries.extend(
            self.custom_paths
                .iter()
                .map(|path| custom_entry(path, home)),
        );

        let mut model = SelectionModel::new(entries);
        for deselected in &self.deselected_paths {
            model.deselect_entry(deselected);
        }
        model
    }
}

/// Builds an [`Entry`] for a remembered Custom path, since Custom paths are the one part of a
/// Selection `scan::scan_taxonomy` never re-discovers (their Category's allowlist is empty by
/// design). Re-rooting mirrors `scan`'s: a path under `home` is `EntryRoot::Home` with a
/// home-relative path; anything else is `EntryRoot::Absolute`. Sensitivity is re-derived from
/// the Safety-net scan so a Custom path pointing at a key file still forces encryption.
fn custom_entry(path: &Path, home: &Path) -> Entry {
    let (root, relative_path) = match path.strip_prefix(home) {
        Ok(relative) => (EntryRoot::Home, relative.to_path_buf()),
        Err(_) => (
            EntryRoot::Absolute,
            path.strip_prefix("/").unwrap_or(path).to_path_buf(),
        ),
    };
    Entry {
        absolute_path: path.to_path_buf(),
        relative_path,
        root,
        category: catalog::CUSTOM_PATHS.name,
        sensitive: scan::is_safety_net_match(path),
    }
}

/// Serializes `profile` to TOML and writes it to `path`, creating parent directories as
/// needed. Overwrites any existing file (the Profile is rewritten each run — CONTEXT.md).
pub fn save(profile: &Profile, path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let serialized =
        toml::to_string_pretty(profile).map_err(|err| CoreError::Profile(err.to_string()))?;
    fs::write(path, serialized).map_err(|source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Loads the Profile at `path`, pruning stale path-typed entries via the injected `exists`
/// check (see [`prune`]).
///
/// A missing file returns `Ok(None)` — the first-run signal (story 61), never an error. Any
/// other read failure, or malformed TOML, is a real error.
pub fn load(
    path: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<Option<LoadedProfile>, CoreError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CoreError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    let profile: Profile =
        toml::from_str(&contents).map_err(|err| CoreError::Profile(err.to_string()))?;
    let (profile, pruned) = prune(profile, exists);
    Ok(Some(LoadedProfile { profile, pruned }))
}

/// Drops every path-typed entry that no longer exists (per the injected `exists`), counting
/// each drop (story 59/60). Custom paths are the clear case; the last Output location is
/// pruned the same way when the drive it named is gone. Never errors on a stale path.
fn prune(mut profile: Profile, exists: impl Fn(&Path) -> bool) -> (Profile, usize) {
    let custom_before = profile.custom_paths.len();
    profile.custom_paths.retain(|path| exists(path));
    let mut pruned = custom_before - profile.custom_paths.len();

    if let Some(location) = &profile.output_location {
        if !exists(location) {
            profile.output_location = None;
            pruned += 1;
        }
    }

    (profile, pruned)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::catalog;
    use std::collections::BTreeSet;

    fn entry(category: &'static str, sensitive: bool, relative: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/home/test/{relative}")),
            relative_path: PathBuf::from(relative),
            root: EntryRoot::Home,
            category,
            sensitive,
        }
    }

    fn scan(category: catalog::Category, entries: Vec<Entry>) -> CategoryScan {
        CategoryScan { category, entries }
    }

    #[test]
    fn toml_round_trip_preserves_every_field() {
        let profile = Profile {
            selected_categories: vec!["SSH".to_string(), "KDE Plasma".to_string()],
            custom_paths: vec![PathBuf::from("/home/test/notes")],
            deselected_paths: vec![PathBuf::from(".local/share/icons/heavy.svg")],
            output_location: Some(PathBuf::from("/run/media/test/USB")),
        };

        let serialized = toml::to_string_pretty(&profile).expect("serialize");
        let parsed: Profile = toml::from_str(&serialized).expect("deserialize");

        assert_eq!(parsed, profile);
    }

    #[test]
    fn omitted_output_location_deserializes_to_none() {
        // A Profile written with no Output location (or one pruned away) must load cleanly,
        // not fail on a missing key.
        let parsed: Profile = toml::from_str("selected_categories = [\"Shell\"]\n").expect("parse");
        assert_eq!(parsed.output_location, None);
        assert_eq!(parsed.selected_categories, vec!["Shell".to_string()]);
    }

    #[test]
    fn prune_drops_and_counts_stale_custom_paths_but_keeps_live_ones() {
        let live = PathBuf::from("/home/test/keep");
        let gone = PathBuf::from("/home/test/deleted");
        let profile = Profile {
            custom_paths: vec![live.clone(), gone],
            ..Profile::default()
        };

        let (pruned_profile, count) = prune(profile, |path| path == live);

        assert_eq!(count, 1);
        assert_eq!(pruned_profile.custom_paths, vec![live]);
    }

    #[test]
    fn prune_drops_a_stale_output_location_and_counts_it() {
        let profile = Profile {
            output_location: Some(PathBuf::from("/run/media/test/UNPLUGGED")),
            ..Profile::default()
        };

        // Nothing exists on this fake disk.
        let (pruned_profile, count) = prune(profile, |_| false);

        assert_eq!(count, 1);
        assert_eq!(pruned_profile.output_location, None);
    }

    #[test]
    fn prune_counts_nothing_when_every_remembered_path_still_exists() {
        let profile = Profile {
            custom_paths: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            output_location: Some(PathBuf::from("/c")),
            ..Profile::default()
        };

        let (_, count) = prune(profile, |_| true);

        assert_eq!(count, 0);
    }

    #[test]
    fn load_returns_none_for_a_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist/profile.toml");

        let loaded = load(&missing, |_| true).expect("load must not error on a missing file");

        assert!(
            loaded.is_none(),
            "a missing Profile is the first-run signal"
        );
    }

    #[test]
    fn save_then_load_round_trips_through_a_real_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config/linux-backup/profile.toml");
        let profile = Profile {
            selected_categories: vec!["SSH".to_string()],
            custom_paths: vec![PathBuf::from("/home/test/notes")],
            deselected_paths: vec![PathBuf::from(".config/kwinrc")],
            output_location: Some(PathBuf::from("/run/media/test/USB")),
        };

        save(&profile, &path).expect("save creates parent dirs and writes");
        // Everything still exists on this fake disk, so nothing is pruned.
        let loaded = load(&path, |_| true).expect("load").expect("present");

        assert_eq!(loaded.pruned, 0);
        assert_eq!(loaded.profile, profile);
    }

    #[test]
    fn selection_round_trips_categories_deselections_and_custom_paths() {
        // A Selection saved to a Profile, then reconstructed against the same scan, must
        // reproduce the same active Entries, deselections, and Custom paths.
        let home = PathBuf::from("/home/test");
        let ssh = scan(
            catalog::SSH,
            vec![entry(catalog::SSH.name, true, ".ssh/id_ed25519")],
        );
        let plasma = scan(
            catalog::KDE_PLASMA,
            vec![
                entry(catalog::KDE_PLASMA.name, false, ".config/kwinrc"),
                entry(
                    catalog::KDE_PLASMA.name,
                    false,
                    ".local/share/icons/heavy.svg",
                ),
            ],
        );
        let scanned = vec![ssh, plasma];

        // Start from a Selection: SSH + KDE Plasma on, one heavy Plasma icon drilled out.
        let mut original = SelectionModel::new(
            scanned
                .iter()
                .flat_map(|s| s.entries.iter().cloned())
                .collect(),
        );
        original.deselect_entry(Path::new(".local/share/icons/heavy.svg"));

        let profile =
            Profile::from_selection(&original, Some(PathBuf::from("/run/media/test/USB")));
        let reconstructed = profile.to_selection_model(&scanned, &home);

        let active_paths = |model: &SelectionModel| -> BTreeSet<PathBuf> {
            model
                .entries()
                .iter()
                .map(|e| e.relative_path.clone())
                .collect()
        };
        assert_eq!(active_paths(&reconstructed), active_paths(&original));
        assert_eq!(
            reconstructed.deselected_paths(),
            original.deselected_paths()
        );
        assert_eq!(
            profile.output_location,
            Some(PathBuf::from("/run/media/test/USB"))
        );
    }

    #[test]
    fn unselected_categories_are_not_reconstructed() {
        let home = PathBuf::from("/home/test");
        let scanned = vec![
            scan(
                catalog::SSH,
                vec![entry(catalog::SSH.name, true, ".ssh/id_ed25519")],
            ),
            scan(
                catalog::USER_DATA,
                vec![entry(catalog::USER_DATA.name, false, "Documents/big.pdf")],
            ),
        ];

        // Only SSH was on last run; User data was not.
        let profile = Profile {
            selected_categories: vec!["SSH".to_string()],
            ..Profile::default()
        };
        let model = profile.to_selection_model(&scanned, &home);

        assert_eq!(model.entries().len(), 1);
        assert_eq!(model.entries()[0].category, catalog::SSH.name);
    }

    #[test]
    fn a_custom_path_under_home_reconstructs_as_a_home_rooted_entry() {
        let home = PathBuf::from("/home/test");
        let profile = Profile {
            custom_paths: vec![PathBuf::from("/home/test/projects/notes.md")],
            ..Profile::default()
        };

        let model = profile.to_selection_model(&[], &home);

        assert_eq!(model.entries().len(), 1);
        let custom = &model.entries()[0];
        assert_eq!(custom.category, catalog::CUSTOM_PATHS.name);
        assert_eq!(custom.root, EntryRoot::Home);
        assert_eq!(custom.relative_path, Path::new("projects/notes.md"));
    }

    #[test]
    fn a_custom_path_outside_home_reconstructs_as_absolute_and_safety_net_stays_sensitive() {
        let home = PathBuf::from("/home/test");
        let profile = Profile {
            custom_paths: vec![PathBuf::from("/etc/ssl/private/server.key")],
            ..Profile::default()
        };

        let model = profile.to_selection_model(&[], &home);

        let custom = &model.entries()[0];
        assert_eq!(custom.root, EntryRoot::Absolute);
        assert_eq!(
            custom.relative_path,
            Path::new("etc/ssl/private/server.key")
        );
        assert!(
            custom.sensitive,
            "a Custom path at a .key file must re-derive as Sensitive via the Safety-net scan"
        );
    }
}
