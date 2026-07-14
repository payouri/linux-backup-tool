# Package Inventory: Capture & Reinstall (Manjaro/Arch)

Research resolving ticket #3. Target platform: Manjaro/Arch. Scope: capture
installed apps as **package-manager lists only**; restore emits reinstall
commands the user runs manually (no auto-install in alpha).

All capture commands below are **read-only** (`-Q` query / `list` / `--list`).
Verified against this machine on 2026-07-15.

## Recommendation table

| Manager | Detect presence | Capture command | Reinstall command |
|---|---|---|---|
| pacman (native explicit) | `command -v pacman` | `pacman -Qqen` | `sudo pacman -S --needed - < pkglist` |
| pacman (foreign / AUR) | `command -v pacman` (+ AUR helper `command -v yay`/`paru`) | `pacman -Qqem` | `yay -S --needed - < aurlist` |
| flatpak | `command -v flatpak` | `flatpak list --app --columns=application,origin` | `flatpak install <origin> <app>` |
| snap | `command -v snap` | `snap list \| awk 'NR>1{print $1, $4}'` | `snap install <name> --channel=<tracking>` |
| cargo | `command -v cargo` | `cargo install --list` | `cargo install <name>` |
| npm (global) | `command -v npm` | `npm ls -g --depth=0 --json` | `npm install -g <name>` |

Detection: run `command -v <bin>` for each manager; a manager can be **present
but empty** (flatpak and cargo were both present with 0 apps on this box), so
presence != non-empty list. Treat an empty capture as valid, not an error.

## Per-manager notes & gotchas

### pacman — split native vs AUR (critical)
- `-Q` queries the **local** DB (installed), not repos. `-q` = names only,
  `-e` = explicitly installed (drop dependencies pulled in transitively),
  `-n` = native (in a sync repo), `-m` = foreign (NOT in any sync repo, i.e.
  AUR / manually installed).
- `pacman -Qqen` → explicit **native** packages. This box: **306**.
  Reinstall: `sudo pacman -S --needed - < pkglist` (reads names from stdin;
  `--needed` skips already-current packages so it is idempotent).
- `pacman -Qqem` → explicit **foreign/AUR** packages. This box: **53**
  (e.g. android-sdk, ankama-launcher, aws-session-manager-plugin).
- **Gotcha — the split matters.** Feeding AUR names to `pacman -S` fails
  (not in repos); feeding native names to `yay` works but is pointless. Keep
  two lists. AUR names must go through an AUR helper (`yay -S --needed -` /
  `paru -S --needed -`); the alpha should detect which helper exists via
  `command -v yay || command -v paru` and note that if none is present the user
  must install one first. `yay` is the helper on this Manjaro box.
- **Gotcha — `-e` vs `-Qqn` (all native).** Use `-e` (explicit) so the list is
  the packages the user chose, not the hundreds of auto-pulled deps; deps come
  back automatically when the explicit set is reinstalled.
- Optional: `pacman -Q` (no flags) captures name+version, but pin the version
  only if you truly need it — see Recommendation on versions below.

### flatpak — capture the remote/origin
- Detect: `command -v flatpak`. This box has flatpak present but **0 apps**.
- `flatpak list --app` limits to user-facing apps (excludes runtimes/platforms,
  which are pulled in as deps anyway). `--columns=application` gives the app ID
  (e.g. `org.gimp.GIMP`); add `,origin` to record which remote it came from.
- **Gotcha — remotes are not portable.** An app ID alone is not reinstallable
  unless the target machine has the same remote configured. `flatpak install`
  wants `<remote> <appid>` (e.g. `flatpak install flathub org.gimp.GIMP`).
  Capture the origin column and, ideally, also record configured remotes
  (`flatpak remotes --columns=name,url`) so restore can re-add `flathub` etc.
  before installing. `--system` vs `--user` installs also differ; default
  restore to the same scope or let the user choose.

### snap — capture the tracking channel
- Detect: `command -v snap`. This box: **21 entries**, mostly base/core snaps
  (bare, core, core18/20/22, plus real apps).
- `snap list` columns: `Name Version Rev Tracking Publisher Notes`. Skip the
  header (`NR>1`) and take col1 (name) + col4 (**Tracking** = the channel, e.g.
  `latest/stable`). `snap list | awk 'NR>1{print $1, $4}'`.
- Reinstall: `snap install <name> --channel=<tracking>`.
- **Gotcha — base snaps as noise.** `core`, `core*`, `bare`, `snapd` are
  runtime bases auto-installed as dependencies of other snaps. Reinstalling the
  real apps pulls their bases automatically, so you can filter these out of the
  user-facing list (keep them only if you want an exact mirror). Snap installs
  need root (sudo) and network.
- **Gotcha — channel required for fidelity.** Omitting `--channel` installs the
  default (`latest/stable`), which may differ from what the user tracked
  (e.g. an app on `beta` or a versioned track like `1.28/stable`).

### cargo — crates.io vs git/path installs
- Detect: `command -v cargo`. This box: **0 installed**.
- `cargo install --list` output: one `name vX.Y.Z:` header line per crate,
  followed by indented binary name(s). Parse the non-indented header lines for
  crate names.
- Reinstall: `cargo install <name>` (pulls latest from crates.io).
- **Gotcha — non-crates.io installs are not captured faithfully.** `--list`
  shows only name+version; it does **not** record whether the binary was
  installed from crates.io, a `--git <url>`, a `--path`, or with `--features`.
  A plain `cargo install <name>` will fetch the crates.io release, which is
  wrong for git/local installs and drops feature flags. Flag this as a known
  alpha limitation (crates.io-only fidelity). Reinstall recompiles from source
  (needs a Rust toolchain + network + build time).

### npm — exclude self and noise
- Detect: `command -v npm`. Capture globals with
  `npm ls -g --depth=0 --json` → `.dependencies` is a map of
  `name -> {version, overridden}`. `--depth=0` avoids listing transitive deps.
- Reinstall: `npm install -g <name>` (optionally `<name>@<version>`).
- **Gotcha — exclude `npm` itself.** `npm` appears in its own global list;
  reinstalling it globally via `npm install -g npm` is redundant/undesirable.
  Filter out the `npm` key. Also consider excluding `corepack`/`node-gyp`/`nopt`
  if they came bundled with Node rather than being user-installed — but the safe
  default is to keep everything except `npm` and let the user prune.
- **Gotcha — `--parseable` is less useful.** It emits install paths, not clean
  name→version, so prefer `--json`. Global prefix differs across nvm/system
  Node; reinstall targets whatever `npm` is active at restore time.

## Cross-cutting gotchas

- **sudo & network on reinstall.** pacman, yay, and snap all need root; every
  manager needs network (and cargo needs a compile toolchain). The alpha only
  *emits* the commands, so this is the user's concern — but the emitted commands
  should include `sudo` where required so they run as-is.
- **Idempotency.** `--needed` (pacman/yay) skips already-installed current
  packages, making re-runs safe. npm/cargo/flatpak/snap installs are naturally
  re-runnable (they no-op or upgrade).
- **Present-but-empty is normal.** flatpak and cargo were present with zero
  apps. Detect the manager, then capture; store an empty list rather than
  skipping the manager.

## Recommendation: do NOT pin versions for the alpha

For the alpha, **capture names only and reinstall latest**. Rationale:
- pacman/AUR are rolling — old versions are usually gone from mirrors, so a
  pinned version often can't be reinstalled at all.
- Version pinning multiplies failure modes (yanked crates, unavailable snap
  revisions, npm version conflicts) for little benefit in a dev-environment
  restore where "current" is normally what the user wants.
- Exceptions where pinning is trivial and worth keeping the captured version:
  npm (`npm install -g name@version`) and cargo (`cargo install name --version
  x.y.z`) accept an exact version cheaply. Snap can pin via a versioned track
  channel. pacman cannot practically pin against rolling repos. Recommend:
  store the observed version as metadata for reference, but emit latest-install
  commands by default, with pinning as a later opt-in.
