//! A throwaway verification harness for the encryption screen (#31), reachable as
//! `linux-backup encrypt-demo`.
//!
//! #31's screen can't produce a Backup dir on its own — it only decides an [`Encryption`] —
//! yet one of its acceptance criteria is that completing it "produces a real encrypted Backup
//! dir when run end-to-end." The full WHAT → encryption → WHERE → RUN chain that will drive it
//! for real is #33; until then this command stands the screen up against `core::backup` over a
//! self-contained synthetic fixture, so the end-to-end path can be exercised now. It builds a
//! one-Entry Sensitive Selection (a fake SSH key under a temp `$HOME`) so Auto-insist triggers
//! deterministically regardless of the running machine's real home, runs the screen, then
//! backs up into a temp Output location and lists the result. Expect this whole module to be
//! deleted when #33 lands the real wizard.

use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use linux_backup_core::archive::ENCRYPTED_SUFFIX;
use linux_backup_core::model::{Entry, EntryRoot, SelectionModel};
use linux_backup_core::packages::NoCommandRunner;
use linux_backup_core::{backup, catalog, Privilege, RunContext};

pub fn run() -> Result<()> {
    let root = unique_temp_dir()?;
    let home = root.join("home");
    let output = root.join("output");
    fs::create_dir_all(&home).with_context(|| format!("create fixture home {}", home.display()))?;
    fs::create_dir_all(&output)
        .with_context(|| format!("create fixture output {}", output.display()))?;

    let key_path = home.join("id_ed25519");
    fs::write(&key_path, b"FAKE SSH PRIVATE KEY - demo fixture only\n")
        .with_context(|| format!("write fixture key {}", key_path.display()))?;

    // Sensitive so `crypto::evaluate` reports encryption_required and the screen actually runs
    // its Auto-insist flow rather than short-circuiting to `Encryption::None`.
    let selection = SelectionModel::new(vec![Entry {
        absolute_path: key_path,
        relative_path: PathBuf::from("id_ed25519"),
        root: EntryRoot::Home,
        category: catalog::SSH.name,
        sensitive: true,
    }]);

    let outcome = crate::encryption_cmd::run(&selection)?;

    let ctx = RunContext {
        home,
        output_location: output,
        privilege: Privilege::NonRoot,
        host: "encrypt-demo".to_string(),
        user: "encrypt-demo".to_string(),
        uid: 1000,
        os: "encrypt-demo".to_string(),
        now: SystemTime::now(),
    };

    let result = backup(&selection, &ctx, &outcome.encryption, &NoCommandRunner)
        .context("core::backup failed")?;

    println!();
    println!("auto_insist_disabled = {}", outcome.auto_insist_disabled);
    println!("Backup dir: {}", result.backup_dir.display());
    let mut encrypted_files = 0usize;
    for dir_entry in fs::read_dir(&result.backup_dir).context("read produced Backup dir back")? {
        let name = dir_entry.context("read Backup dir entry")?.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(ENCRYPTED_SUFFIX) {
            encrypted_files += 1;
        }
        println!("  - {name}");
    }
    println!("{encrypted_files} file(s) carry the {ENCRYPTED_SUFFIX} suffix");

    Ok(())
}

/// A unique, freshly-created temp directory. Not using `tempfile` (a dev-dependency only):
/// this is a throwaway demo command, so a pid+nanos-suffixed dir under the system temp is
/// enough without adding a runtime dependency for it.
fn unique_temp_dir() -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "linux-backup-encrypt-demo-{}-{nanos}",
        process::id()
    ));
    fs::create_dir_all(&dir).with_context(|| format!("create temp dir {}", dir.display()))?;
    Ok(dir)
}
