//! Package list capture (#25): querying each package manager present on the machine and
//! parsing its stdout into [`PackageInfo`] entries for the Manifest's `packages` field.
//!
//! Per ADR-0004, `core` reads nothing ambient — but shelling out to a package manager is
//! not an ambient-fact read like `$HOME`/hostname/the clock; it's a system-boundary
//! operation (a real subprocess, real machine state). The parent spec (#17)'s Testing
//! Decisions keep system-boundary edges off the round-trip seam and exercise them manually
//! rather than in CI. So this module draws its own, narrower seam: [`CommandRunner`] is the
//! one thing that isn't unit-tested (its real implementation, [`SystemCommandRunner`], just
//! shells out); every parser below is a pure `&str -> Vec<PackageInfo>` function, unit-
//! tested directly against captured/fake command output.
//!
//! Six managers are captured: pacman (native), yay/AUR, flatpak (with origin), snap (with
//! tracking channel), cargo, and npm globals. A manager not present on the machine (its
//! program missing, or failing to run) is skipped — never a run failure (story: "a missing
//! manager is skipped without failing the run").
//!
//! Versions are informational only (story 48): nothing here or downstream treats a captured
//! version as a pin. There is deliberately no `pin_version` field or similar — reinstalling
//! on a rolling-release distro must not chase an unavailable exact version.

use std::collections::BTreeMap;
use std::process::Command;

use crate::manifest::PackageInfo;

/// The Manifest's `packages` map key for each captured manager.
pub const PACMAN: &str = "pacman";
pub const AUR: &str = "aur";
pub const FLATPAK: &str = "flatpak";
pub const SNAP: &str = "snap";
pub const CARGO: &str = "cargo";
pub const NPM: &str = "npm";

/// The one system-boundary seam this module draws: "run this program with these args and
/// give me its stdout, or tell me it isn't there." Real subprocess invocation
/// ([`SystemCommandRunner`]) is exercised manually against real hardware, per the parent
/// spec's Testing Decisions — not in CI. Everything downstream of a `CommandRunner` (the
/// parsers, and `capture_all`'s per-manager dispatch/fallback logic) is pure and unit-
/// tested against a fake implementation.
pub trait CommandRunner {
    /// Runs `program` with `args` and returns its captured stdout, or `None` if `program`
    /// isn't present / didn't run successfully. `None` is not an error — the caller treats a
    /// missing manager as "skip it," never as a reason to fail the run.
    fn run(&self, program: &str, args: &[&str]) -> Option<String>;
}

/// The real [`CommandRunner`]: shells out via [`std::process::Command`]. Not unit-tested
/// (see the module doc) — it has no logic of its own beyond "try to run it."
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// A [`CommandRunner`] that reports every program as absent. Used where package-list
/// capture should be skipped entirely without touching the real system — e.g. the
/// round-trip seam (`crates/core/tests/roundtrip.rs`), which per the parent spec's Testing
/// Decisions must stay deterministic across machines regardless of which package managers
/// happen to be installed on whatever machine runs the test suite.
pub struct NoCommandRunner;

impl CommandRunner for NoCommandRunner {
    fn run(&self, _program: &str, _args: &[&str]) -> Option<String> {
        None
    }
}

/// Queries every known package manager via `runner` and parses whatever stdout comes back,
/// returning a map keyed by the constants above (only present managers get an entry).
///
/// AUR capture prefers `yay -Qm` (yay understands AUR/foreign packages specifically) and
/// falls back to `pacman -Qem` when `yay` isn't installed — both produce the same "name
/// version" line shape, so [`parse_name_version_lines`] handles either. Native pacman
/// packages are queried with `-Qen` (explicitly installed, native/repo-only — the `n` flag
/// is what keeps AUR/foreign packages out of this section; see
/// [`parse_name_version_lines`]'s doc for why the issue's literal `-Qqen` isn't used
/// verbatim).
pub fn capture_all(runner: &dyn CommandRunner) -> BTreeMap<String, Vec<PackageInfo>> {
    let mut packages = BTreeMap::new();

    if let Some(out) = runner.run("pacman", &["-Qen"]) {
        packages.insert(PACMAN.to_string(), parse_name_version_lines(&out));
    }

    if let Some(out) = runner.run("yay", &["-Qm"]) {
        packages.insert(AUR.to_string(), parse_name_version_lines(&out));
    } else if let Some(out) = runner.run("pacman", &["-Qem"]) {
        packages.insert(AUR.to_string(), parse_name_version_lines(&out));
    }

    if let Some(out) = runner.run(
        "flatpak",
        &["list", "--app", "--columns=application,version,origin"],
    ) {
        packages.insert(FLATPAK.to_string(), parse_flatpak(&out));
    }

    if let Some(out) = runner.run("snap", &["list"]) {
        packages.insert(SNAP.to_string(), parse_snap(&out));
    }

    if let Some(out) = runner.run("cargo", &["install", "--list"]) {
        packages.insert(CARGO.to_string(), parse_cargo_install_list(&out));
    }

    if let Some(out) = runner.run("npm", &["list", "-g", "--depth=0", "--json"]) {
        packages.insert(NPM.to_string(), parse_npm_global(&out));
    }

    packages
}

/// Parses the "name version" line-per-package shape shared by `pacman -Qen`/`-Qem` and
/// `yay -Qm` (e.g. `acpi 1.8-2`).
///
/// Deviation from issue #25's literal text: the issue names `pacman -Qqen` for native
/// packages, but `-Qq` (quiet) suppresses the version column entirely, leaving bare package
/// names — the acceptance criteria require name *and* version, so this crate runs `-Qen`
/// (no quiet) instead. `-Qem` follows the same reasoning for AUR/foreign.
fn parse_name_version_lines(output: &str) -> Vec<PackageInfo> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (name, version) = line.split_once(' ')?;
            Some(PackageInfo {
                name: name.trim().to_string(),
                version: version.trim().to_string(),
                origin: None,
                channel: None,
            })
        })
        .collect()
}

/// Parses `flatpak list --app --columns=application,version,origin`'s tab-separated rows
/// into `PackageInfo { name, version, origin: Some(_), .. }`. A missing version or origin
/// field (flatpak sometimes leaves these blank) degrades gracefully rather than dropping the
/// row.
fn parse_flatpak(output: &str) -> Vec<PackageInfo> {
    output
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let mut fields = line.split('\t');
            let name = fields.next()?.trim().to_string();
            let version = fields.next().unwrap_or("").trim().to_string();
            let origin = fields
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(PackageInfo {
                name,
                version,
                origin,
                channel: None,
            })
        })
        .collect()
}

/// Parses `snap list`'s fixed-width-ish, whitespace-separated table (`Name Version Rev
/// Tracking Publisher Notes`) into `PackageInfo { name, version, channel: Some(tracking),
/// .. }`. Skips the `Name ...` header row wherever it appears (normally first, but this
/// tolerates its absence too).
fn parse_snap(output: &str) -> Vec<PackageInfo> {
    output
        .lines()
        .filter(|line| !line.trim_start().starts_with("Name"))
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            let name = fields.next()?.to_string();
            let version = fields.next().unwrap_or("").to_string();
            let _rev = fields.next();
            let channel = fields.next().map(str::to_string);
            Some(PackageInfo {
                name,
                version,
                origin: None,
                channel,
            })
        })
        .collect()
}

/// Parses `cargo install --list`'s output:
/// ```text
/// cargo-audit v0.17.6:
///     cargo-audit
/// somepkg v0.1.0 (https://github.com/x/y#abcdef123456):
///     somebin
/// ```
/// Only the un-indented header lines (`name vVERSION[ (origin)]:`) carry a package; the
/// indented binary-name lines underneath are ignored.
fn parse_cargo_install_list(output: &str) -> Vec<PackageInfo> {
    output
        .lines()
        .filter(|line| !line.starts_with(' ') && !line.starts_with('\t'))
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_suffix(':')?;
            let mut parts = line.split_whitespace();
            let name = parts.next()?.to_string();
            let version_token = parts.next()?;
            let version = version_token
                .strip_prefix('v')
                .unwrap_or(version_token)
                .to_string();
            let origin_raw = parts.collect::<Vec<_>>().join(" ");
            let origin = if origin_raw.is_empty() {
                None
            } else {
                Some(
                    origin_raw
                        .trim_matches(|c| c == '(' || c == ')')
                        .to_string(),
                )
            };
            Some(PackageInfo {
                name,
                version,
                origin,
                channel: None,
            })
        })
        .collect()
}

/// Parses `npm list -g --depth=0 --json`'s `{"dependencies": {"<name>": {"version": ...}}}`
/// shape, excluding `npm` itself (story: "npm globals ... excluding npm itself" — npm always
/// lists itself as a global dependency of itself, which isn't a package the user installed).
fn parse_npm_global(output: &str) -> Vec<PackageInfo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let Some(deps) = value.get("dependencies").and_then(|d| d.as_object()) else {
        return Vec::new();
    };

    deps.iter()
        .filter(|(name, _)| name.as_str() != "npm")
        .filter_map(|(name, info)| {
            let version = info.get("version")?.as_str()?.to_string();
            Some(PackageInfo {
                name: name.clone(),
                version,
                origin: None,
                channel: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    // --- parser unit tests, each against realistic captured/fixture output ---

    #[test]
    fn parses_pacman_native_name_version_lines() {
        // Captured from `pacman -Qen` on a real Manjaro machine.
        let output = "acpi 1.8-2\nacpid 2.0.34-2\nadobe-source-han-sans-cn-fonts 2.005-2\n";
        let parsed = parse_name_version_lines(output);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].name, "acpi");
        assert_eq!(parsed[0].version, "1.8-2");
        assert!(parsed[0].origin.is_none());
        assert!(parsed[0].channel.is_none());
        assert_eq!(parsed[2].name, "adobe-source-han-sans-cn-fonts");
        assert_eq!(parsed[2].version, "2.005-2");
    }

    #[test]
    fn parses_aur_name_version_lines_from_yay_or_pacman_qem() {
        // Captured from `yay -Qm` / `pacman -Qem` (identical line shape) on a real machine.
        let output = "android-sdk 26.1.1-2\nankama-launcher 3.11.7-1\n";
        let parsed = parse_name_version_lines(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "android-sdk");
        assert_eq!(parsed[0].version, "26.1.1-2");
        assert_eq!(parsed[1].name, "ankama-launcher");
        assert_eq!(parsed[1].version, "3.11.7-1");
    }

    #[test]
    fn parse_name_version_lines_skips_blank_lines() {
        let parsed = parse_name_version_lines("acpi 1.8-2\n\n\nacpid 2.0.34-2\n");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parses_flatpak_tab_separated_columns_with_origin() {
        let output = "org.mozilla.firefox\t130.0\tflathub\ncom.spotify.Client\t1.2.3\tflathub\n";
        let parsed = parse_flatpak(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "org.mozilla.firefox");
        assert_eq!(parsed[0].version, "130.0");
        assert_eq!(parsed[0].origin.as_deref(), Some("flathub"));
        assert_eq!(parsed[1].name, "com.spotify.Client");
        assert_eq!(parsed[1].origin.as_deref(), Some("flathub"));
    }

    #[test]
    fn parses_flatpak_missing_version_or_origin_gracefully() {
        // Some flatpak apps report an empty version field.
        let output = "org.example.NoVersion\t\tflathub\n";
        let parsed = parse_flatpak(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "org.example.NoVersion");
        assert_eq!(parsed[0].version, "");
        assert_eq!(parsed[0].origin.as_deref(), Some("flathub"));
    }

    #[test]
    fn parses_snap_list_with_tracking_channel() {
        // Captured from `snap list` on a real machine (header row + fixed-column table).
        let output = "Name    Version                      Rev    Tracking       Publisher    Notes\n\
                       bare    1.0                          5      latest/stable  canonical**  base\n\
                       core20  20260204                     2999   latest/stable  canonical**  base\n";
        let parsed = parse_snap(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "bare");
        assert_eq!(parsed[0].version, "1.0");
        assert_eq!(parsed[0].channel.as_deref(), Some("latest/stable"));
        assert_eq!(parsed[1].name, "core20");
        assert_eq!(parsed[1].version, "20260204");
        assert_eq!(parsed[1].channel.as_deref(), Some("latest/stable"));
    }

    #[test]
    fn parse_snap_tolerates_missing_header() {
        let output = "bare 1.0 5 latest/stable canonical** base\n";
        let parsed = parse_snap(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "bare");
    }

    #[test]
    fn parses_cargo_install_list_simple_and_git_origin() {
        let output = "cargo-audit v0.17.6:\n    cargo-audit\nsomepkg v0.1.0 (https://github.com/x/y#abcdef123456):\n    somebin\n";
        let parsed = parse_cargo_install_list(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "cargo-audit");
        assert_eq!(parsed[0].version, "0.17.6");
        assert!(parsed[0].origin.is_none());
        assert_eq!(parsed[1].name, "somepkg");
        assert_eq!(parsed[1].version, "0.1.0");
        assert_eq!(
            parsed[1].origin.as_deref(),
            Some("https://github.com/x/y#abcdef123456")
        );
    }

    #[test]
    fn parse_cargo_install_list_ignores_indented_binary_lines() {
        let output = "ripgrep v13.0.0:\n    rg\n";
        let parsed = parse_cargo_install_list(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "ripgrep");
    }

    #[test]
    fn parses_npm_global_json_excluding_npm_itself() {
        // Captured from `npm list -g --depth=0 --json` on a real machine.
        let output = r#"{
          "name": "lib",
          "dependencies": {
            "nativefier": { "version": "52.0.0", "overridden": false },
            "npm": { "version": "11.16.0", "overridden": false },
            "typescript": { "version": "4.8.3", "overridden": false }
          }
        }"#;
        let mut parsed = parse_npm_global(output);
        parsed.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "nativefier");
        assert_eq!(parsed[0].version, "52.0.0");
        assert_eq!(parsed[1].name, "typescript");
        assert_eq!(parsed[1].version, "4.8.3");
        assert!(parsed.iter().all(|p| p.name != "npm"));
    }

    #[test]
    fn parse_npm_global_handles_missing_dependencies_key() {
        let parsed = parse_npm_global(r#"{"name": "lib"}"#);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_npm_global_handles_invalid_json() {
        let parsed = parse_npm_global("not json");
        assert!(parsed.is_empty());
    }

    // --- capture_all dispatch/fallback logic, against a fake CommandRunner ---

    struct FakeCommandRunner {
        responses: RefCell<HashMap<(String, Vec<String>), String>>,
    }

    impl FakeCommandRunner {
        fn new() -> Self {
            Self {
                responses: RefCell::new(HashMap::new()),
            }
        }

        fn with(self, program: &str, args: &[&str], output: &str) -> Self {
            self.responses.borrow_mut().insert(
                (
                    program.to_string(),
                    args.iter().map(ToString::to_string).collect(),
                ),
                output.to_string(),
            );
            self
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, args: &[&str]) -> Option<String> {
            let key = (
                program.to_string(),
                args.iter().map(ToString::to_string).collect::<Vec<_>>(),
            );
            self.responses.borrow().get(&key).cloned()
        }
    }

    #[test]
    fn no_command_runner_reports_every_manager_absent() {
        let packages = capture_all(&NoCommandRunner);
        assert!(
            packages.is_empty(),
            "NoCommandRunner should yield no captured managers at all"
        );
    }

    #[test]
    fn capture_all_skips_absent_managers_without_failing() {
        // Only pacman "present"; everything else absent. Must not panic or error.
        let runner = FakeCommandRunner::new().with("pacman", &["-Qen"], "acpi 1.8-2\n");
        let packages = capture_all(&runner);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains_key(PACMAN));
        assert!(!packages.contains_key(AUR));
        assert!(!packages.contains_key(FLATPAK));
        assert!(!packages.contains_key(SNAP));
        assert!(!packages.contains_key(CARGO));
        assert!(!packages.contains_key(NPM));
    }

    #[test]
    fn capture_all_prefers_yay_for_aur_when_present() {
        let runner = FakeCommandRunner::new()
            .with("yay", &["-Qm"], "from-yay 1.0-1\n")
            .with("pacman", &["-Qem"], "from-pacman 2.0-1\n");
        let packages = capture_all(&runner);
        let aur = packages.get(AUR).expect("aur key present");
        assert_eq!(aur.len(), 1);
        assert_eq!(aur[0].name, "from-yay");
    }

    #[test]
    fn capture_all_falls_back_to_pacman_qem_when_yay_absent() {
        let runner = FakeCommandRunner::new().with("pacman", &["-Qem"], "from-pacman 2.0-1\n");
        let packages = capture_all(&runner);
        let aur = packages.get(AUR).expect("aur key present");
        assert_eq!(aur.len(), 1);
        assert_eq!(aur[0].name, "from-pacman");
    }

    #[test]
    fn capture_all_wires_every_manager_when_all_present() {
        let runner = FakeCommandRunner::new()
            .with("pacman", &["-Qen"], "acpi 1.8-2\n")
            .with("yay", &["-Qm"], "android-sdk 26.1.1-2\n")
            .with(
                "flatpak",
                &["list", "--app", "--columns=application,version,origin"],
                "org.mozilla.firefox\t130.0\tflathub\n",
            )
            .with(
                "snap",
                &["list"],
                "Name  Version  Rev  Tracking       Publisher  Notes\nbare  1.0      5    latest/stable  canonical  base\n",
            )
            .with("cargo", &["install", "--list"], "ripgrep v13.0.0:\n    rg\n")
            .with(
                "npm",
                &["list", "-g", "--depth=0", "--json"],
                r#"{"dependencies": {"typescript": {"version": "4.8.3"}, "npm": {"version": "11.16.0"}}}"#,
            );

        let packages = capture_all(&runner);
        assert_eq!(packages.len(), 6);
        assert_eq!(packages[PACMAN].len(), 1);
        assert_eq!(packages[AUR].len(), 1);
        assert_eq!(packages[FLATPAK].len(), 1);
        assert_eq!(packages[SNAP].len(), 1);
        assert_eq!(packages[CARGO].len(), 1);
        assert_eq!(packages[NPM].len(), 1);
    }
}
