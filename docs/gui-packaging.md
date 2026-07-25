# GUI Linux packaging

How `crates/gui` ships on Linux, per [ADR-0008](adr/0008-gui-tauri-entry-point-core-boundary.md)
Decision 4 and [#97](https://github.com/payouri/linux-backup-tool/issues/97)'s Linux-packaging
section (built by [#104](https://github.com/payouri/linux-backup-tool/issues/104)).

## Two ship formats, one canonical

- **Flatpak is the canonical distributable.** The manifest —
  [`crates/gui/flatpak/tools.linux_backup.gui.yml`](../crates/gui/flatpak/tools.linux_backup.gui.yml)
  — targets `org.gnome.Platform`/`org.gnome.Sdk`, which ships a pinned, sandboxed
  `webkit2gtk-4.1` (plus `javascriptcoregtk-4.1`, `gtk3`, `soup3` at matching versions). This is
  what resolves the cross-distro WebKitGTK-version risk the [stack
  survey](research/2026-07-21-gui-stack-survey.md) raised: the shipped app never touches
  whatever webkit the host distro happens to have.
- **AppImage is the fallback** for users who won't or can't use Flatpak. It runs against
  whatever WebKitGTK the host provides, so the version-fragility the Flatpak pin resolves
  survives on this path — accepted per ADR-0008 as the cost of offering a portable
  single-file alternative.
- No `.deb`/`.rpm`/other Tauri bundler target is produced; `crates/gui/tauri.conf.json`'s
  `bundle.targets` is narrowed to `["appimage"]` so a plain `cargo tauri build` (no
  `--bundles` flag) can't accidentally emit one.

## Building each format

### Flatpak

```sh
# One-time, per lockfile change — regenerate whenever Cargo.lock or
# crates/gui/ui/package-lock.json changes. Pinned to a specific flatpak-builder-tools commit
# (it ships no tagged releases) — keep this ref in sync with
# .github/workflows/packaging.yml's flatpak job.
REF=737c0085912f9f7dabf9341d4608e2a77a51a73a
curl -sSfL -o /tmp/flatpak-cargo-generator.py \
  "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$REF/cargo/flatpak-cargo-generator.py"
pip3 install --user aiohttp pyyaml tomlkit
pip3 install --user "git+https://github.com/flatpak/flatpak-builder-tools.git@$REF#subdirectory=node"

python3 /tmp/flatpak-cargo-generator.py Cargo.lock -o crates/gui/flatpak/cargo-sources.json
flatpak-node-generator npm crates/gui/ui/package-lock.json \
  -o crates/gui/flatpak/node-sources.json

flatpak-builder --force-clean --user \
  --repo=crates/gui/flatpak/repo \
  crates/gui/flatpak/build \
  crates/gui/flatpak/tools.linux_backup.gui.yml
```

`crates/gui/flatpak/cargo-sources.json` / `node-sources.json` are gitignored — they're a
lockfile-derived cache, generated fresh in CI right before the sandboxed build
(`.github/workflows/packaging.yml`'s `flatpak` job) via the
[`flatpak-builder-tools`](https://github.com/flatpak/flatpak-builder-tools) generators,
never committed. This is what lets the actual `flatpak-builder` sandbox build with **zero
network access**: every crate and npm package it needs is pre-fetched and described by
sha256 in those two files before the sandbox door closes. Inside the sandbox, `cargo` is
pointed at its vendored config via an explicit `CARGO_HOME=/run/build/linux-backup-gui/cargo`
(cargo doesn't auto-discover the generator's bare `cargo/config`, unlike a project-local
`.cargo/config.toml`), and `npm ci` is pointed at the vendored cacache cache via
`--cache flatpak-node/npm-cache`.

### AppImage

```sh
cd crates/gui/ui && npm ci && npm run build && cd -
cargo tauri build --manifest-path crates/gui/Cargo.toml --bundles appimage
# -> target/release/bundle/appimage/*.AppImage
```

## Dev-time webkit2gtk-4.1 dependency

`cargo tauri dev` (and any plain `cargo build`/`cargo test` touching `crates/gui`) links
against and runs against **the host's** `webkit2gtk-4.1`/`javascriptcoregtk-4.1` — the same
Tauri v2 Ubuntu/Debian prerequisites CI's `appimage` job installs
(`libwebkit2gtk-4.1-dev`, `libjavascriptcoregtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev`,
`libssl-dev`, `libayatana-appindicator3-dev`, `patchelf`). Install the equivalent package
names for your distro to work on the GUI locally.

**The Flatpak sandbox's pinned webkit is only ever present in the shipped app** — it plays no
role in the dev loop. `cargo tauri dev` against host webkit is the whole of local GUI
development; there is no "run inside the Flatpak sandbox" dev workflow. This asymmetry is the
entire reason Decision 4 exists: the pin buys the *shipped* app cross-distro consistency, at
the cost of every dev box (and CI) still needing a working host webkit to build and run the
Rust/Tauri toolchain at all.

## Known limitation: `location.rs` inside the sandbox

`crates/core/src/location.rs`'s Output-location enumeration shells out to `lsblk`/`udisksctl`
as host subprocesses. A Flatpak sandbox does not expose host binaries even with
`--filesystem=host` (that grants file I/O, not `exec`); reaching them needs
`flatpak-spawn --host` via the Flatpak portal (`--talk-name=org.freedesktop.Flatpak`, already
present in the manifest as forward-looking scaffolding), which `core` does not yet use. This
is out of scope for #104 (packaging) — tracked as a follow-up for whoever picks up making
`location.rs`'s subprocess calls sandbox-aware.

## HITL: no repo license yet

`tools.linux_backup.gui.metainfo.xml` deliberately omits `<project_license>` — the repo has
no `LICENSE` file and no Cargo `license` field to draw one from, so inventing one (even a
plausible-looking placeholder like `MIT`) would be a false claim about terms nobody has
actually chosen, not a packaging detail. This is a repo-level decision for a human to make,
not something this packaging effort should default on: **adopt a license for the repo**, then
add the corresponding `<project_license>` tag here.

## What's verified where

Authoring this (manifest, CI, `tauri.conf.json`, this doc) happened without a local
`flatpak`/`flatpak-builder` install, no `org.gnome.Platform` runtime, and no
`appimagetool`/`linuxdeploy` (Tauri's bundler fetches those on first use, over network) — so
locally this could only be validated for:

- YAML/JSON/XML well-formedness (the manifest, the workflow, `tauri.conf.json`, the metainfo).
- The manifest's shape against Tauri's own [documented Flatpak
  recipe](https://v2.tauri.app/distribute/flatpak/) and the
  [`flatpak-builder-tools`](https://github.com/flatpak/flatpak-builder-tools) node/cargo
  generator conventions (the bare-filename source-splicing syntax, the `--offline` install
  flags).
- That the Rust workspace gates (`cargo fmt --check` / `clippy` / `test`) still see a valid
  `tauri-build`-consumable `tauri.conf.json`.
- Against live upstream (fetched directly, not from memory): that the pinned
  `flatpak-builder-tools` commit exists and contains both generators; that `cargo/`'s
  generator is still the standalone script the manifest/CI expect (CLI shape, its
  `CARGO_HOME="cargo"` constant and the `cargo/config` file it writes); and that `node/`'s
  generator is now the pip-installable `flatpak_node_generator` package (console script
  `flatpak-node-generator`, install via `pip install`/`pipx install
  git+...#subdirectory=node`) rather than the single-file script the workflow used to `curl`
  (that restructuring is exactly what made the original `curl` 404). Its cache-directory
  convention (`flatpak-node/npm-cache`, relative to the build root) was cross-checked against
  its provider source and README, not run end-to-end.

**Rust toolchain**: the Flatpak does *not* use `org.freedesktop.Sdk.Extension.rust-stable`. That
extension ships rustc 1.89, under the workspace's `rust-version = "1.95"` floor (which `sysinfo`
alone already forces), so `cargo build` refused to start; no branch of it carries a 1.95+ rustc.
The manifest vendors the official standalone Rust tarball instead — pinned by version and sha256,
fetched by `flatpak-builder` outside the sandbox, unpacked with `install.sh` into the build prefix
so the compile stays offline. It tracks stable (currently 1.97.1) rather than 1.95.0, matching the
AppImage job so both shipped bundles come out of the same compiler; 1.95 is the floor CI's `msrv`
job enforces, not the version releases are built with.

**Only CI can confirm**: that `org.gnome.Platform`/`org.gnome.Sdk`//48 and the
`node24` SDK extension branch actually resolve together; that the vendored Rust tarball's
`install.sh --components=…` invocation lands a working toolchain on `PATH`; that the generated
`cargo-sources.json`/`node-sources.json` — with the `CARGO_HOME`/`--cache` wiring above —
produce a build that completes offline inside the sandbox; that the resulting Flatpak
actually launches the GUI; and that `cargo tauri build --bundles appimage` (with the
now-cwd-independent `beforeBuildCommand`) produces a working AppImage. The manifest's
`org.gnome.Platform` version pin is explicitly a build-time detail (ADR-0008/#97) — bump both
the `runtime-version` and the paired SDK extension branches together if CI reports either
extension unavailable.
