//! Hand-rolled argv parsing for `linux-backup`'s CLI surface.
//!
//! Not using `clap` here: the alpha's entire CLI grammar is a handful of subcommands
//! (`backup`, `restore <dir> [--verify] [--overwrite]`, plus the standalone `select`/`where`
//! screens), the richest of which is one positional argument and two boolean flags — well
//! within what a short loop over `std::env::args()` handles directly. Pulling in a
//! derive/builder-based arg-parsing crate for this would trade a new dependency (and its
//! compile-time cost) for no real ergonomics win yet. Revisit this choice if/when the grammar
//! grows richer flag sets past what's comfortable to hand-parse.

use std::path::PathBuf;

/// Parsed arguments for `linux-backup restore <dir> [--verify] [--overwrite]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreArgs {
    pub backup_dir: PathBuf,
    /// Story 57: opt-in checksum re-verification pass after restore.
    pub verify: bool,
    /// `--overwrite`: pre-consent to overwriting on the command line, for scripted/AFK runs
    /// (issue #37). The interactive overwrite-confirm screen *is* the whole-run overwrite
    /// consent (#76), so passing this flag skips it — an encrypted Backup still opens on the
    /// passphrase-unlock screen (the passphrase is required regardless), a plaintext one launches
    /// the run straight away. Defaults to `false`, in which case the flow opens on the
    /// overwrite-confirm screen as usual. Either way `core::restore` never silently overwrites:
    /// the consent is explicit, given either up front here or on that screen.
    pub overwrite: bool,
}

/// The supported top-level command shapes today. An enum (rather than returning
/// `RestoreArgs` directly from `parse_args`) so a future `backup` subcommand has somewhere
/// to land without reshaping this function's return type. `Select` (#30) is exactly that
/// kind of interim landing spot: it's the WHAT screen standing alone as its own subcommand
/// so it can be run and verified against a real account ahead of `backup` wiring it into
/// the full WHAT -> encryption -> WHERE -> REVIEW -> RUN wizard (#31/#32/…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Restore(RestoreArgs),
    /// The full forward-only backup wizard (#33): WHAT → [encryption] → WHERE → REVIEW → RUN,
    /// chaining the three standalone screens into the real end-to-end `backup` flow.
    Backup,
    Select,
    /// The WHERE screen (#32) standing alone as its own subcommand, the same interim
    /// landing spot as `Select`: runnable and verifiable against a real account's drives
    /// ahead of `backup` chaining it into the full wizard (#33).
    Where,
}

const USAGE: &str = "usage: linux-backup backup\n       linux-backup restore <dir> [--verify] [--overwrite]\n       linux-backup select\n       linux-backup where";

/// Parses a full argv (excluding argv[0], the program name) into a [`Command`].
pub fn parse_args<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    let subcommand = iter
        .next()
        .ok_or_else(|| format!("missing command\n{USAGE}"))?;

    match subcommand.as_ref() {
        "backup" => parse_no_arg_command(iter, Command::Backup),
        "restore" => parse_restore_args(iter),
        "select" => parse_select_args(iter),
        "where" => parse_no_arg_command(iter, Command::Where),
        other => Err(format!("unknown command '{other}'\n{USAGE}")),
    }
}

/// `select` takes no arguments — any extra token is rejected the same way an unexpected
/// second positional argument is for `restore`, rather than silently ignored.
fn parse_select_args<I, S>(rest: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Some(extra) = rest.into_iter().next() {
        return Err(format!("unexpected argument '{}'\n{USAGE}", extra.as_ref()));
    }
    Ok(Command::Select)
}

/// Accept a subcommand that takes no arguments, rejecting any extra token the same way
/// `parse_select_args` does. Returns `command` when `rest` is empty.
fn parse_no_arg_command<I, S>(rest: I, command: Command) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Some(extra) = rest.into_iter().next() {
        return Err(format!("unexpected argument '{}'\n{USAGE}", extra.as_ref()));
    }
    Ok(command)
}

fn parse_restore_args<I, S>(rest: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut backup_dir = None;
    let mut verify = false;
    let mut overwrite = false;

    for arg in rest {
        match arg.as_ref() {
            "--verify" => verify = true,
            "--overwrite" => overwrite = true,
            other if backup_dir.is_none() => backup_dir = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument '{other}'\n{USAGE}")),
        }
    }

    let backup_dir = backup_dir.ok_or_else(|| format!("missing <dir> argument\n{USAGE}"))?;
    Ok(Command::Restore(RestoreArgs {
        backup_dir,
        verify,
        overwrite,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_command(dir: &str, verify: bool, overwrite: bool) -> Command {
        Command::Restore(RestoreArgs {
            backup_dir: PathBuf::from(dir),
            verify,
            overwrite,
        })
    }

    #[test]
    fn parses_bare_restore_with_dir_only() {
        let parsed = parse_args(["restore", "/tmp/my-backup"]).expect("should parse");
        assert_eq!(parsed, restore_command("/tmp/my-backup", false, false));
    }

    #[test]
    fn parses_verify_flag_after_the_dir() {
        let parsed = parse_args(["restore", "/tmp/my-backup", "--verify"]).expect("should parse");
        assert_eq!(parsed, restore_command("/tmp/my-backup", true, false));
    }

    #[test]
    fn parses_verify_flag_before_the_dir() {
        let parsed = parse_args(["restore", "--verify", "/tmp/my-backup"]).expect("should parse");
        assert_eq!(parsed, restore_command("/tmp/my-backup", true, false));
    }

    #[test]
    fn parses_both_flags_together_in_any_order() {
        let parsed = parse_args(["restore", "--overwrite", "/tmp/my-backup", "--verify"])
            .expect("should parse");
        assert_eq!(parsed, restore_command("/tmp/my-backup", true, true));
    }

    #[test]
    fn rejects_missing_command() {
        let err = parse_args(std::iter::empty::<&str>()).expect_err("should reject");
        assert!(err.contains("missing command"));
    }

    #[test]
    fn rejects_unknown_command() {
        let err = parse_args(["frobnicate"]).expect_err("should reject");
        assert!(err.contains("unknown command 'frobnicate'"));
    }

    #[test]
    fn parses_bare_backup_with_no_arguments() {
        let parsed = parse_args(["backup"]).expect("should parse");
        assert_eq!(parsed, Command::Backup);
    }

    #[test]
    fn rejects_backup_with_an_unexpected_argument() {
        let err = parse_args(["backup", "extra"]).expect_err("should reject");
        assert!(err.contains("unexpected argument 'extra'"));
    }

    #[test]
    fn rejects_restore_with_no_dir() {
        let err = parse_args(["restore", "--verify"]).expect_err("should reject");
        assert!(err.contains("missing <dir>"));
    }

    #[test]
    fn rejects_restore_with_a_second_positional_argument() {
        let err = parse_args(["restore", "/tmp/a", "/tmp/b"]).expect_err("should reject");
        assert!(err.contains("unexpected argument '/tmp/b'"));
    }

    #[test]
    fn parses_bare_select_with_no_arguments() {
        let parsed = parse_args(["select"]).expect("should parse");
        assert_eq!(parsed, Command::Select);
    }

    #[test]
    fn rejects_select_with_an_unexpected_argument() {
        let err = parse_args(["select", "extra"]).expect_err("should reject");
        assert!(err.contains("unexpected argument 'extra'"));
    }

    #[test]
    fn parses_bare_where_with_no_arguments() {
        let parsed = parse_args(["where"]).expect("should parse");
        assert_eq!(parsed, Command::Where);
    }

    #[test]
    fn rejects_where_with_an_unexpected_argument() {
        let err = parse_args(["where", "extra"]).expect_err("should reject");
        assert!(err.contains("unexpected argument 'extra'"));
    }
}
