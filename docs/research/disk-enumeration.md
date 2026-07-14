# Disk / mount enumeration + free space

Research for ticket [#13](https://github.com/payouri/linux-backup-tool/issues/13) (part of #1).
Scope: the output-location pick-list (#5) needs, per candidate row: **mount point,
available free space, filesystem type**, and ideally a **removable/external flag**. This
decides only the *enumeration mechanism*; #5's behaviour is locked regardless.

Verified on **2026-07-15** on the target machine (Manjaro / Arch, KDE Plasma, kernel 6.12,
`rustc`/`cargo` 1.95.0) by compiling and running each option, and by reading the crate
source in the local cargo registry.

## Recommendation

**Use the `sysinfo` crate (`Disks` API).** On this machine it returns exactly the two
block-backed mounts with correct `mount_point`, `file_system`, `total_space`,
`available_space`, and `is_removable`, having already filtered out the 34 snap squashfs
loop mounts and 6 tmpfs that a hand-roll would have to strip by hand. Reading its Linux
source settles the central unknown: **it does surface udisks/KDE removable mounts under
`/run/media/$USER/…`** (see below), and its `is_removable` flag is exactly the
external-drive signal #13 asked for. The hand-roll (`/proc/self/mountinfo` + `statvfs`) is
a viable fallback with no heavy dep, but it reimplements filtering that `sysinfo` already
gets right — including a non-obvious NFS/CIFS `statvfs`-hang guard — for no MVP benefit.

| Concern | Choice | Version | One-line rationale | Health (downloads / last release) |
|---|---|---|---|---|
| Disk/mount enumeration + free space | **`sysinfo`** | 0.39.6 | One `Disks::new_with_refreshed_list()` call yields mount point, fstype, total/avail bytes, and a USB `is_removable` flag; snap/tmpfs/pseudo-fs pre-filtered; `/run/media` mounts kept. | 167M total / 37M recent (90d); last release 2026-07-09. Maintained by GuillaumeGomez (rustlang contributor). |
| Fallback: available bytes | `rustix` **or** `nix` | 1.1.4 / 0.31.3 | `statvfs(3)` wrapper if hand-rolling; both expose `statvfs`. `rustix` is the higher-traffic, more actively released of the two. | rustix 946M total / 223M recent, 2026-02-22 · nix 664M total / 147M recent, 2026-05-11 |

Net dependency line:

```toml
sysinfo = { version = "0.39", default-features = false, features = ["disk"] }
```

`disk` is already in `sysinfo`'s `default` feature set, but disabling defaults and opting
into just `disk` avoids pulling the `component`/`network`/`system`/`user` collectors we
don't use.

## The central question, answered: does `sysinfo` surface udisks/KDE USB mounts?

**No USB was attached during this research** — verified: `/run/media/` is empty, `/media`
does not exist, `/mnt` is empty, and `/dev/disk/by-id/` contains *no* `usb-*` symlinks
(only the internal NVMe). So the removable-mount path was **not exercised at runtime**.
The answer below is therefore read off the crate's source, not an observed USB row.

Reading `sysinfo-0.39.6/src/unix/linux/disk.rs`, enumeration works like this:

1. **Input is `/proc/mounts`**, not physical block devices and not `/sys` scanning
   (`get_all_utf8_data("/proc/mounts", 16_385)`, line 179). This is the same mount table a
   hand-roll would read — so anything the kernel lists as mounted is a candidate, including
   a udisks-mounted USB stick.
2. **The `/run` exclusion explicitly exempts `/run/media`** (line 427):
   ```rust
   (fs_file.starts_with("/run") && !fs_file.starts_with("/run/media"))
   ```
   `/run/*` is dropped, *except* `/run/media/...`. KDE/udisks2 auto-mounts removable media
   at `/run/media/$USER/<label>`, so such a mount is **kept**, provided its filesystem type
   isn't itself on the ignore list (vfat/exfat/ntfs/ext4 are all fine; see filter below).
3. **`is_removable` comes from `/dev/disk/by-id/`** (line 363): entries whose filename
   starts with `usb-` are canonicalized, and a disk is flagged removable when its device
   node matches. A udisks-mounted USB partition (e.g. `/dev/sdb1`) has a
   `by-id/usb-…-part1` symlink, so it would report `is_removable = true`.

**Conclusion:** a plugged-in, udisks/KDE-mounted USB stick under `/run/media/$USER/…`
**will** appear in `Disks::list()` with `is_removable = true`. This is a source-level
certainty (the `/run/media` carve-out and the `usb-` by-id scan are unambiguous), pending a
future runtime confirmation with an actual stick attached. Caveat: a USB drive that is
*present but not mounted* is **not** returned — `sysinfo` only ever lists mounts, so the
pick-list shows removable targets only once the user (or udisks) has mounted them.

## What it actually returns on this machine

`Disks::new_with_refreshed_list()` (sysinfo 0.39.6, and identically on 0.37.2) returned
**exactly 2 disks**:

```
name="/dev/nvme0n1p9" mount="/"         fs="ext4" removable=false kind=SSD total=600353062912 avail=74220748800
name="/dev/nvme0n1p7" mount="/boot/efi" fs="vfat" removable=false kind=SSD total=1071628288   avail=1071308800
```

`avail` on `/` = 74.22 GB ≈ 69 GiB, matching `df -h` ("69G Dispo"). Meanwhile the raw mount
table had **62 entries** — 34 squashfs, 6 tmpfs, plus sysfs/proc/cgroup2/devtmpfs/etc. and
only 2 real block-backed filesystems. `sysinfo` filtered all the noise for us.

## How the filtering works (why the noise disappears)

The `/proc/mounts` filter in `get_all_list()` drops an entry when its fstype or mount point
is in an ignore set (lines 407–428):

- **Always dropped fstypes:** `rootfs`, `sysfs`, `proc`, `devtmpfs`, `cgroup`, `cgroup2`,
  `pstore`, **`squashfs`** (explicitly commented "for snaps"), `rpc_pipefs`, `iso9660`,
  `devpts`, `hugetlbfs`, `mqueue`.
- **`tmpfs`** dropped unless the `linux-tmpfs` feature is enabled.
- **`cifs` / `nfs` / `nfs4` / `autofs`** dropped unless `linux-netdevs` is enabled — the
  source comment states the reason: *"calling statvfs on a mounted CIFS or NFS or through
  autofs may hang, when they are mounted with option: hard."* This is a real hazard a
  hand-roll must reproduce.
- **Mount-point prefixes dropped:** `/sys*`, `/proc*`, and `/run*` **except `/run/media`**.
- Free space then comes from `libc::statvfs(mount_point)` — `f_bsize * f_blocks` for total,
  `f_bsize * f_bavail` for available, plus `ST_RDONLY` for read-only.

So on defaults you get "real, writable-target-worthy" filesystems, with removable media
under `/run/media` included.

## The hand-roll alternative (`/proc/self/mountinfo` + `statvfs`)

Viable, dependency-light, but you take on the filtering `sysinfo` already does. Note
`sysinfo` reads `/proc/mounts`; #13 proposed `/proc/self/mountinfo`. Both list the same
mounts; `mountinfo` additionally carries the mount root and a per-mount ID, useful for
detecting bind mounts. `statvfs(3)` is available via `rustix::fs::statvfs` or
`nix::sys::statvfs::statvfs`. Caveats you would own:

- **Snap squashfs + tmpfs noise:** on this machine that is 40 of 62 entries. You must
  filter by fstype yourself or the pick-list is unusable.
- **NFS/CIFS/autofs `statvfs` hang:** `statvfs` on a hard-mounted network share can block
  indefinitely. `sysinfo` skips these by default; a hand-roll must too (skip by fstype
  before calling `statvfs`, or bound it).
- **Bind mounts / duplicate entries:** the same device can be mounted at several points;
  `mountinfo` lets you spot binds via the mount-root field. `sysinfo` instead de-dupes on
  the exact `(mount_point, device_name, file_system)` triple, so distinct bind targets
  still show as separate rows there too — decide whether the pick-list wants that.
- **`statvfs` on a foreign mount:** reading free space needs the mount point to be
  traversable by the current user; a `statvfs` on a mount you can't reach returns an error
  (or the value is simply unavailable) and must be handled, not unwrapped.
- **No removable flag:** you'd re-implement the `/dev/disk/by-id/usb-*` scan yourself to
  match `sysinfo`'s `is_removable`.

## Sources

- `sysinfo` 0.39.6 (and 0.37.2) source: `src/unix/linux/disk.rs` — `get_all_list`
  (`/proc/mounts` read, ignore-list, `/run/media` carve-out), `new_disk` (`by-id/usb-*`
  removable detection), `load_statvfs_values` (`libc::statvfs`). Read from the local cargo
  registry; repo <https://github.com/GuillaumeGomez/sysinfo>.
- `sysinfo` `Cargo.toml`: `default = ["component","disk","network","system","user"]`;
  `disk`, `linux-tmpfs`, `linux-netdevs` features.
- Runtime probe: a throwaway crate calling `Disks::new_with_refreshed_list()`, built and
  run on this machine (2 disks returned, as above).
- Machine state: `lsblk`, `df -hT`, `/proc/self/mountinfo` (62 mounts; 34 squashfs, 6
  tmpfs), `/run/media` (empty), `/dev/disk/by-id/` (no `usb-*` symlinks).
- crates.io API (`/api/v1/crates/{sysinfo,nix,rustix}`), fetched 2026-07-15.
</content>
</invoke>
