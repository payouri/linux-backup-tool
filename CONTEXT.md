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
The user's chosen set of Categories, Custom paths, and Drill-down deselections for a Run. The single authoritative model of what will be backed up; both interfaces are thin views over it and neither holds a Selection of its own.
_Avoid_: choices, config, options

**Smart default**:
A pre-computed starting state the user can override — which Categories start checked, and the encryption Auto-insist. Defaults are computed fresh each Run (never persisted for encryption).
_Avoid_: preset, recommendation

**Drill-down**:
The activity of descending from a checked Category through its Entries — and recursively into subdirectories to any depth — to deselect individual Entries or whole subtrees (the heavy Plasma `icons/`/`wallpapers/` case). Deselections persist in the Profile. Performed in the Explorer.
_Avoid_: expand, detail view

**Explorer**:
The navigable tree surface a Drill-down happens in, presenting Categories and their Entries to any depth with each node's Partial selection state. One concept, two renderings — a pane in the terminal app, a tree in the desktop app.
_Avoid_: browser, file picker, tree view (for the surface itself)

**Subtree deselection**:
A Drill-down deselection applied to a directory at any depth, excluding every Entry beneath it (prefix-based — it prunes the whole subtree). The coarser of the two deselection granularities; the other is a single-Entry deselection. Exclusion is prefix-wins: re-inclusion cancels a deselection only at the exact node it was applied — there is no per-child carve-out of an excluded parent. Persisted (for historical reasons) in the Profile's `deselected_roots`, whose "root" now means any subtree, not only an allowlist root.
_Avoid_: exclude rule, ignore pattern, negation

**Partial selection**:
A directory node whose descendants are a mix of selected and withheld Entries — the middle of the three-valued All / Partial / None state the Explorer shows for a node. Derived from the Selection, never stored.
_Avoid_: indeterminate, mixed, half-checked

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

## The wizard

**Wizard**:
The ordered walk that assembles a backup Run: WHAT (the Selection), encryption, WHERE (the Output location), then Review. Both interfaces present the same four Steps in the same order.
_Avoid_: flow, setup, onboarding

**Step**:
One stage of the Wizard — WHAT, encryption, WHERE, or Review. Named in caps when it stands for the stage itself (a "WHAT edit"), lowercase when read as prose. Distinct from a Phase, which belongs to a Run in progress.
_Avoid_: page, screen, tab, phase

**Completion frontier**:
The furthest Step the user has completed. Everything behind it is revisitable at will; everything beyond it is locked until the current Step is satisfied — which is why editing WHAT can pull the frontier back.
_Avoid_: progress, furthest step, unlocked steps

**Invalidation**:
The rule that a WHAT edit exposing new Sensitive content reopens an already-settled encryption decision, marking that Step unsatisfied rather than silently keeping a decision the user made about different content. Targeted, never a blanket reset of later Steps.
_Avoid_: reset, dirty, stale

## Runs

**Run**:
One invocation of backup or restore, from launch to Run outcome. The unit a Phase, a Cancellation, and a Run outcome all belong to; also the unit the Profile and the Smart defaults are computed per.
_Avoid_: job, task, operation, session, execution

**Phase**:
A coarse, user-visible stage of a Run in progress — Scanning, Compressing, Encrypting, Finalizing for a backup (Encrypting only when the Backup is actually encrypted); Verifying archive, Restoring for a restore. Deliberately coarser than the internal steps it covers. Distinct from a Step, which belongs to the Wizard.
_Avoid_: stage, step, state, progress

**Run outcome**:
How a Run ended. A backup reports Success, Cancelled, or Failed; a restore reports Success or Failed and carries its Cancellation as a flag on a Success rather than as an outcome of its own. Either may instead report Internal — an unexpected crash, stated plainly and without alarming detail. Exactly one per Run, and always reported: a Run never simply stops.
_Avoid_: result, status, exit code

**Cancellation**:
The user abandoning a Run in progress, taken at the Run's own checkpoints rather than interrupting work already underway. A cancelled backup leaves nothing behind — its Backup dir is removed exactly as on a failure — while a cancelled Restore keeps every file that already landed and reports itself partial. Offered in the desktop app only; the terminal app has no cancel affordance yet.
_Avoid_: abort, stop, interrupt

**Point of no return**:
The moment a backup Run starts committing Archive bytes: everything before it is cancellable, nothing after it is. The desktop app withdraws the cancel affordance as the Run crosses it, rather than leaving a control that would appear to cancel and change nothing. A Restore has no point of no return — it stays cancellable to its last Entry, because it never promised all-or-nothing in the first place.
_Avoid_: commit point, deadline

**Occupied Backup dir**:
A Backup dir that already exists where this Run's would be written. A hard refusal before any filesystem work — never merged into, never overwritten. The one collision-shaped case that is *not* a Collision, which is a restore-time concept.
_Avoid_: collision, conflict, duplicate

## On-disk artifacts

**Backup**:
The on-disk result of a Run: a Backup dir containing one Archive and one Manifest. The unit `restore` consumes.
_Avoid_: snapshot (reserved for the v1 dedup store), dump

**Backup dir**:
The per-Run directory `<location>/linux-backup-<host>-<timestamp>/` that holds the Archive and Manifest. Created fresh each Run, never merged into or written over — an Occupied Backup dir is refused.
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

**Backup recognition**:
Reading a directory the user has picked to answer two questions before anything else happens: is this a Backup dir, and is it encrypted. What lets the passphrase prompt appear only when the Backup actually needs one.
_Avoid_: validation, probe, detection, scan (that belongs to backup)

**Collision**:
An existing file where a restored Entry would land. Always negotiated, never a refusal: the user is asked once for the whole list and answers yes or no for all of it.
_Avoid_: conflict, clash, overwrite (that's the answer, not the situation)

**Skipped collision**:
A Collision the user declined, leaving the existing file untouched. A deliberate outcome recorded in the Restore's report — never a failure. The chosen one of the three ways an Entry can go unwritten: a Skipped collision is asked for, a restore failure is not, and a Vanished Entry never reached the Archive at all.
_Avoid_: rejected, refused, ignored

**Restore failure**:
One Entry that could not be written, recorded and reported without ending the Run. The asymmetry that defines Restore: a backup aborts on its first error, a Restore carries on and hands back the list.
_Avoid_: error, partial failure

**Ownership restore**:
Putting each restored file back under the uid/gid the Manifest recorded for it — attempted only when the Restore runs as root. A non-root Restore leaves every file owned by the restoring user and says so, rather than failing. The ownership half of what Re-rooting does for paths.
_Avoid_: chown, permissions (those are modes, restored separately), uid mapping

**Package list**:
A Category and a Manifest section: per-package-manager inventories (pacman, yay/AUR, flatpak, snap, cargo, npm globals) captured as names (versions informational only).
_Avoid_: package inventory (use "Package list"), app list

**Reinstall commands**:
The `reinstall.sh` a Restore writes from the Package lists — unpinned, never auto-run. The user runs it themselves.
_Avoid_: install script, provisioning

**Vanished**:
An Entry that existed at scan time but was gone (ENOENT) by the time it was read — a benign cache/temp race. Skipped-with-note, recorded in the Manifest's `vanished` array; never aborts a Run. Distinct from a real read error, which aborts.
_Avoid_: missing, skipped, lost

## Persistence

**Profile**:
The single remembered state (`profile.toml`) that pre-fills the next Run's Selection — Categories, Custom paths, Drill-down deselections, and last Output location. Overwritten each Run. Encryption is never persisted.
_Avoid_: config, settings, preferences, saved state
