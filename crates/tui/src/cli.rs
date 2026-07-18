//! Hand-rolled argv parsing for `linux-backup`'s CLI surface.
//!
//! Not using `clap` here: the alpha's entire CLI grammar is one subcommand
//! (`restore <dir> [--verify] [--overwrite]`) with a single positional argument and two
//! boolean flags — well within what a short loop over `std::env::args()` handles directly.
//! Pulling in a derive/builder-based arg-parsing crate for a two-token grammar would trade a
//! new dependency (and its compile-time cost) for no real ergonomics win yet. Revisit this
//! choice if/when a `backup` subcommand (or richer flag set) lands and the grammar grows
//! past what's comfortable to hand-parse.

use std::path::PathBuf;

/// Parsed arguments for `linux-backup restore <dir> [--verify] [--overwrite]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreArgs {
    pub backup_dir: PathBuf,
    /// Story 57: opt-in checksum re-verification pass after restore.
    pub verify: bool,
    /// Story 54's CLI-scope stand-in for the interactive "confirm once" collision prompt
    /// (that prompt itself is TUI wizard territory — see `restore_cmd`'s doc comment for
    /// the full reasoning). Defaults to `false`: a collision is refused and reported as a
    /// failure rather than silently overwritten, matching `core::restore`'s own safe
    /// default when `overwrite_existing` is unset.
    pub overwrite: bool,
}

/// The one supported top-level command shape today. An enum (rather than returning
/// `RestoreArgs` directly from `parse_args`) so a future `backup` subcommand has somewhere
/// to land without reshaping this function's return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Restore(RestoreArgs),
}

const USAGE: &str = "usage: linux-backup restore <dir> [--verify] [--overwrite]";

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
        "restore" => parse_restore_args(iter),
        other => Err(format!("unknown command '{other}'\n{USAGE}")),
    }
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
        let err = parse_args(["backup"]).expect_err("should reject");
        assert!(err.contains("unknown command 'backup'"));
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
}
