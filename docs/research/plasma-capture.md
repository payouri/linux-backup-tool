# Research #2 — Capturing & restoring the KDE Plasma layout

Ticket: [#2](https://github.com/payouri/linux-backup-tool/issues/2) (part of #1).
Target verified on this machine: Manjaro/Arch, KDE Plasma 6, `konsave 2.3.0` at `/usr/bin/konsave`.

## Recommendation (do this)

**Copy raw config files directly. Do not shell out to `konsave`, and do not depend on it.**

The decisive finding: `konsave` *is* a plain file copy driven by a YAML manifest. Its
`apply` step is literally `copy(profile/<section>, original_location)` — no transforms,
no merging, no `kwriteconfig`/dbus calls (verified in its source, below). So the alpha's
"untar to original paths" restore model already reproduces konsave's entire mechanism.
Adopting the tool would only add a Python runtime dependency and an external process to
manage, while inheriting its incomplete Plasma 6 coverage — for zero behavioural gain.

Concretely, the alpha should:
1. Ship a **curated allowlist** of Plasma paths (list below) — borrow konsave's list, which
   is a good community-maintained starting point, but own it in our manifest.
2. During backup, `tar` those paths (that exist) into the archive, preserving relative
   paths under `$HOME`.
3. During restore, untar to `$HOME`. This is exactly konsave's `apply`, minus the tool.
4. **Restore requires a Plasma re-login** to take full effect (konsave prints the same
   warning). Surface this to the user; don't try to hot-reload.

Trade-offs:
- Plain copy is transparent, tool-free, and matches the existing archive design (single
  `.tar.zst` + `manifest.json`, whole-archive `age`). Restore stays "untar to path".
- Risk: config files store some **absolute paths** (notably wallpaper images and custom
  theme locations). If a referenced asset lives outside `$HOME` (e.g. `/usr/share/wallpapers`)
  it is not in our archive; on restore the path resolves only if the same package is
  installed. This is a shared limitation — konsave has it too — and is acceptable for alpha.
- Risk: restoring config from a *different* Plasma version can be partially ignored. Same
  target version (Plasma 6 → Plasma 6) is the supported alpha path.

## Evidence

### 1. `konsave` is just a manifest-driven file copy (source read locally)

`/usr/lib/python3.14/site-packages/konsave/funcs.py`:
- `save_profile()` reads `conf.yaml` `save:` sections and, per entry,
  `copy(source, dest)` / `shutil.copy` into `~/.config/konsave/profiles/<name>`.
- `export()` copies the `save:` entries **plus** the `export:` entries (themes, icons,
  wallpapers) into a folder tree, then zips it to a `.knsv` (a plain zip).
- `import` unzips; `apply_profile()` does `copy(profile/<section>, <original location>)`
  straight back — no post-processing beyond a "log out / log in" notice.

So export/import/apply == tar/untar with a copy-back. Nothing Plasma-aware happens.

`konsave --help` confirms the surface: `-s save`, `-a apply`, `-e export`, `-i import`,
`-r remove`, `-w wipe`. Version: `Konsave: 2.3.0`.

### 2. What konsave's manifest covers (`~/.config/konsave/conf.yaml` on this box)

`save.configs` ($CONFIG_DIR): gtk-2.0/3.0/4.0, kate, Kvantum, latte, dolphinrc, katerc,
konsolerc, kcminputrc, kdeglobals, kglobalshortcutsrc, klipperrc, krunnerrc,
kscreenlockerrc, ksmserverrc, kwinrc, kwinrulesrc, **plasma-org.kde.plasma.desktop-appletsrc**,
plasmarc, plasmashellrc, gtkrc(-2.0), lattedockrc, breezerc, oxygenrc, lightlyrc, ksplashrc,
khotkeysrc, systemsettingsrc, kded5rc, kconf_updaterc, kscreenrc, plasma-systemmonitorrc,
spectaclerc, plasmanotifyrc, plasma-localerc, plasma_calendar_holiday_regions,
xdg-desktop-portal-kderc, kwinoutputconfig.json.
(This box's conf.yaml has been hand-annotated with "Plasma 6.x compatibility" additions —
i.e. stock konsave did not ship them.)

`save.app_layouts` ($HOME/.local/share/kxmlgui5): dolphin, konsole, yakuakerc, systemsettings,
spectacle, plasma-systemmonitor. `save.autostart`: `$CONFIG_DIR/autostart`.

`export.share_folder` ($SHARE_DIR): plasma, kwin, konsole, fonts, color-schemes, aurorae,
icons, wallpapers, yakuake. `export.home_folder`: .fonts, .themes, .icons.

### 3. What konsave misses / caveats

- **kded6rc / Plasma 6 file drift**: stock konsave targets `kded5rc`; Plasma 6 uses `kded6rc`.
  Its Plasma 6 support is community-tracked and incomplete — upstream issue
  [Prayag2/konsave#106](https://github.com/Prayag2/konsave/issues/106). Panel-layout restore
  regressions on Plasma 6 are a known complaint.
- **`~/.config/kdedefaults/`** (default color-scheme / look-and-feel pointer files) is **not**
  in the manifest — this exists on this machine (kdeglobals, kwinrc, kscreenlockerrc,
  ksplashrc, plasmarc, kcminputrc, package) and affects which global theme is active.
- **Wallpaper image files** are only captured if under `~/.local/share/wallpapers`. Wallpapers
  set from `/usr/share/wallpapers` or arbitrary paths are referenced by absolute path in
  `plasma-org.kde.plasma.desktop-appletsrc` and are not archived.
- The `save` vs `export` split is a konsave sharing concept (small profile vs full bundle);
  a backup tool should just capture **both** unconditionally.

### 4. Plasma paths verified to exist on THIS machine (read-only `ls`)

`~/.config/` (files):
`plasma-org.kde.plasma.desktop-appletsrc` (20K), `plasmashellrc`, `plasmarc`, `plasmaparc`,
`kwinrc` (6.5K), `kwinrulesrc`, `kwinoutputconfig.json`, `kglobalshortcutsrc` (17K),
`kdeglobals` (4.2K), `kscreenlockerrc`, `ksplashrc`, `ksmserverrc`, `kcminputrc`, `kgammarc`,
`kded5rc`, `kded_device_automounterrc`, `dolphinrc`, `katerc` (+kate* siblings), `konsolerc`,
`konsolesshconfig`, `klipperrc`, `krunnerrc`, `kwalletrc`, `kactivitymanagerd*rc`,
`plasma-localerc`, `plasma-nm`, `plasmanotifyrc`, `plasma_calendar_holiday_regions`,
`powermanagementprofilesrc`, `systemsettingsrc`, `touchpadxlibinputrc`, `xdg-desktop-portal-kderc`.

`~/.config/` (dirs): `KDE/`, `kde.org/`, `kdedefaults/`, `plasma-workspace/`, `kate/`,
`kdeconnect/`, `autostart/` (Flameshot, Nextcloud, yakuake, matray, slack .desktop entries).

`~/.local/share/` (dirs, sizes): `plasma` (22M), `plasmashell` (4K), `plasma-systemmonitor`
(28K), `color-schemes` (20K), `aurorae` (1.4M), `icons` (**226M**), `wallpapers` (4.5M),
`konsole` (8K), `kxmlgui5` (24K: ark/dolphin/kcalc/konsole), `kscreen` (260K), `fonts`,
`kactivitymanagerd`, `plasma_icons`, `plasma_notes`, `applications`.

Note: `~/.local/share/icons` is 226M — an allowlist should let the user opt out of bundled
icon themes to keep archive size sane; the small per-app config dirs are cheap.

## Suggested alpha allowlist (own it in manifest.json, seeded from konsave)

Config files under `~/.config`: the Plasma/`k*rc` set verified above, plus
**`kdedefaults/`** (the konsave gap). Config dirs: `autostart/`, `plasma-workspace/`,
`kdedefaults/`, `kde.org/`, `KDE/`.
Data under `~/.local/share`: `plasma`, `plasmashell`, `plasma-systemmonitor`, `kxmlgui5`,
`kscreen`, `color-schemes`, `aurorae`, `konsole`; and (size-gated / opt-in) `icons`,
`wallpapers`, `fonts`.
Restore = untar to `$HOME`; then prompt the user to log out/in.

## Sources
- Local: `/usr/lib/python3.14/site-packages/konsave/funcs.py`, `~/.config/konsave/conf.yaml`,
  `konsave --help` / `--version` (2.3.0), read-only `ls` of `~/.config` and `~/.local/share`.
- [Prayag2/konsave#106 — Plasma 6 support](https://github.com/Prayag2/konsave/issues/106)
- [KDE Discuss — Panel config file location](https://discuss.kde.org/t/panel-config-file-location/30764)
- [konsave man page](https://linuxcommandlibrary.com/man/konsave)
