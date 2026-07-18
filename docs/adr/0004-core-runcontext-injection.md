# Core receives a full `RunContext`; it reads nothing from the ambient system

**Status:** Accepted · **Date:** 2026-07-17

Every `core` backup/restore/scan entry point takes a single injected `RunContext { home, output_location, privilege, host, user, uid, os, now }` and reads **nothing** from the ambient system on its own — no `std::env::var("HOME")`, no `dirs::home_dir()`, no `hostname`, no `getuid()`, no clock. `main` (in `crates/tui`) fills the context from the real machine; tests pass fixed values. Anything core would otherwise learn by asking the OS enters through this one struct.

This extends spec [#17](https://github.com/payouri/linux-backup-tool/issues/17), which injects only roots (`$HOME`, output location) and privilege. We inject the whole run context — including `host`, `user`, `uid`, `os`, and `now` — because those same fields land in the Manifest (`created_at`, `host`, `user`, `uid`, `os`) and in the Backup dir name (`linux-backup-<host>-<timestamp>`). If core read them from the environment, the round-trip test could neither assert the Manifest exactly nor be deterministic across machines and runs.

This is the same principle as [ADR-0001](0001-core-tui-split-no-dedup-trait.md)'s core/UI boundary, drawn one line further: if core reading `$HOME` breaks testability, so does core reading the hostname or the clock — so it reads none of them.

## Consequences

The alpha's one round-trip test (`crates/core/tests/roundtrip.rs`) becomes fully deterministic: with a fixed `RunContext` and a Fixture environment of known bytes/modes, the entire Manifest and the Backup dir name are assertable exactly, and the non-root uid/gid-restore fallback (story 53) is tested by passing `privilege: NonRoot` — no CI-as-root required.

Two boundaries this deliberately does **not** move:

- `RunContext.privilege` governs core's uid/gid-restore *logic* only. It does **not** change whether the real OS lets the real process read a byte — so the fail-fast-on-permission-denied test (story 38) still needs a real `0000` file and must skip when the process euid is 0. Injected privilege is not a filesystem sandbox.
- `RunContext.now` sets the Manifest timestamp and dir name, but **not** the per-file mtimes the `tar` crate embeds from the filesystem. `archive_sha256` therefore stays non-deterministic across runs and is asserted as a property (restore's pre-flight verification passes), never as a pinned literal.

A contributor will be tempted to "simplify" by reading `$HOME`/`hostname`/`SystemTime::now()` directly inside core. Don't — it re-introduces the ambient coupling this ADR exists to forbid and silently breaks the round-trip's determinism. If a new fact about the run is needed, add a field to `RunContext`.
