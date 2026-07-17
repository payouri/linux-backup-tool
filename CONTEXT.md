# linux-backup-tool

Single-user Linux **environment** backup and restore: capture a user's configs, keys, shell, KDE Plasma layout, package lists, and hand-picked paths into one portable archive, and put them back. This glossary is the project's ubiquitous language — use these exact terms in code, issues, and tests.

## Content model

**Environment**:
The thing this tool backs up — the *customization layer* of a user account (configs, keys, shell setup, Plasma layout, package lists, custom paths), as opposed to bulk user data. The content boundary is environment-only.
_Avoid_: dotfiles (too narrow), user data (that's one specific, default-off Category), files/stuff (too vague)

**Fixture environment**:
A synthetic, tempdir-rooted, disposable stand-in for a real Environment, built by tests to exercise backup/restore. Distinct from an Environment: it is never a user's actual account, it carries only known files/modes/sensitivity so the round-trip can assert against exactly what was written, and it is thrown away when the test ends.
_Avoid_: test env, fake home, mock (reserve "mock" for behavior doubles, which this is not)

**Category**:
One of the ten named groups a path can belong to (e.g. SSH, KDE Plasma, Shell, Program configs, Package lists, User data, Custom paths). Every backed-up path lives in exactly one Category. Each carries a Curated allowlist, a default on/off, and per-entry sensitivity.
_Avoid_: source, group, section, type

**Curated allowlist**:
The explicit, hand-maintained set of Entries that defines a Category's contents — never a whole-directory copy and never a heuristic. Owned in the Manifest. A path not on any allowlist is only backed up as a Custom path.
_Avoid_: whitelist, include list, filter

**Entry**:
One path on a Category's Curated allowlist — the unit the user drops during Drill-down and the unit the Manifest records. In the Manifest an Entry is called a **root** (a "selected root") and carries a **root-tag** (see Re-rooting).
_Avoid_: item, file, path (when the allowlist unit is meant)

**Custom path**:
A path the user adds by hand, outside every Curated allowlist. Its Category ("Custom paths") is always shown and starts empty. Scope is privilege-aware: any path as root, else under `$HOME`.
_Avoid_: extra path, user path, manual entry

**User data**:
The Category for bulk XDG data directories (Documents, Downloads, …). Default-OFF, carries a size warning. Distinct from Environment as a whole — it is the one large-data Category the boundary otherwise excludes.

## Selection & smart defaults

**Selection**:
The user's chosen set of Categories, Custom paths, and Drill-down deselections for a run. The core's authoritative model of what will be backed up; the TUI is a thin view over it.
_Avoid_: choices, config, options

**Smart default**:
A pre-computed starting state the user can override — which Categories start checked, and the encryption Auto-insist. Defaults are computed fresh each run (never persisted for encryption).
_Avoid_: preset, recommendation

**Drill-down**:
Descending from a checked Category into its Entries to deselect individual ones (the heavy Plasma `icons/`/`wallpapers/` case). Deselections persist in the Profile.
_Avoid_: expand, detail view

**Hide-empty**:
The scan-first rule: a discoverable Category with no matching paths on this machine is hidden from Selection. Custom paths is exempt (always shown, as the add affordance).
_Avoid_: collapse, filter-out

## Sensitivity & encryption

**Sensitive**:
A property marking credential-bearing content, from any of three origins — a **sensitive Category** (SSH/GPG/Cloud-creds, default-ON), a **per-entry sensitivity tag** (e.g. some Agents Entries), or the Safety-net scan. Any sensitivity in the Selection triggers Auto-insist.
_Avoid_: secret (except the crate/type), private, protected

**Safety-net scan**:
The backup-time pass that flags Entries by credential-like name/extension (`credential`/`secret`/`token`/`password`, `.pem`/`.key`/`.p12`/`.pfx`, `id_rsa`/`id_ed25519`, `.netrc`), catching sensitivity the declared tags miss. A strict superset of the declared-sensitive set.
_Avoid_: secret scan, lint

**Auto-insist**:
The policy that forces whole-archive encryption on when the Selection contains anything Sensitive. Overridable, but only behind a second confirmation naming the categories about to land in plaintext.
_Avoid_: force-encrypt, require

## On-disk artifacts

**Backup**:
The on-disk result of a run: a Backup dir containing one Archive and one Manifest. The unit `restore` consumes.
_Avoid_: snapshot (reserved for the v1 dedup store), dump

**Backup dir**:
The per-run directory `<location>/linux-backup-<host>-<timestamp>/` that holds the Archive and Manifest. Created fresh each run; it is the **collision unit** — the run refuses if it already exists.
_Avoid_: backup folder, output folder

**Archive**:
The single `.tar.zst` file (`.tar.zst.age` when encrypted) holding the backed-up bytes. tar → zstd → age, age outermost on disk.
_Avoid_: tarball, backup file, image

**Manifest**:
The `manifest.json` sidecar (`.manifest.json.age` when the Archive is encrypted) describing the Backup: schema/tool version, host/user/home, the selected Entries, per-file checksums, and inline package lists. Mirrors the Archive's encryption all-or-nothing.
_Avoid_: index, catalog, metadata file

**Output location**:
The directory the user picks to hold the Backup dir — `$HOME` or a writable mount. A location, never the Backup dir itself.
_Avoid_: destination, target, drive

## Restore

**Restore**:
Reading a Backup dir and writing its Entries back to their original paths (whole-archive, restore-all). Home-rooted Entries are re-based under the current user's `$HOME`.
_Avoid_: recovery, unpack, extract (except for the tar step)

**Re-rooting**:
Best-effort portability: an Entry tagged `root: "home"` is re-based under the restoring user's `$HOME`; `root: "absolute"` is restored verbatim. This is what makes cross-machine/cross-user Restore possible.
_Avoid_: remapping, relocation

**Package list**:
A Category and a Manifest section: per-package-manager inventories (pacman, yay/AUR, flatpak, snap, cargo, npm globals) captured as names (versions informational only).
_Avoid_: package inventory (use "Package list"), app list

**Reinstall commands**:
The `reinstall.sh` a Restore writes from the Package lists — unpinned, never auto-run. The user runs it themselves.
_Avoid_: install script, provisioning

**Vanished**:
An Entry that existed at scan time but was gone (ENOENT) by the time it was read — a benign cache/temp race. Skipped-with-note, recorded in the Manifest's `vanished` array; never aborts a run. Distinct from a real read error, which aborts.
_Avoid_: missing, skipped, lost

## Persistence

**Profile**:
The single remembered state (`profile.toml`) that pre-fills the next run's Selection — Categories, Custom paths, Drill-down deselections, and last Output location. Overwritten each run. Encryption is never persisted.
_Avoid_: config, settings, preferences, saved state
