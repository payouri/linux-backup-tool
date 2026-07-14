# Rust crate landscape for the alpha

Research for ticket [#4](https://github.com/payouri/linux-backup-tool/issues/4) (part of #1).
Scope: alpha TUI tool that backs up/restores a Linux *environment* into one `.tar.zst`
archive + sidecar `manifest.json`, with smart-default whole-archive `age` encryption.
Bias: MVP simplicity, healthy/well-maintained crates.

Versions and stats confirmed via `cargo search` and the crates.io API on **2026-07-14**.

## Recommended crate set

| Concern | Crate | Version | One-line rationale | Health (downloads / last release) |
|---|---|---|---|---|
| Interactive prompts (TUI) | **`inquire`** | 0.9.4 | Ships `MultiSelect` (with defaults + filtering), `Password`, and `Text` prompts — covers all three alpha interactions with ~no event-loop code. | 17.1M total / 5.5M recent; last release 2026-02-24. Actively maintained. |
| tar archiving | **`tar`** | 0.4.46 | De-facto pure-Rust tar reader/writer; preserves permissions and symlinks, streams to any `Write`. | 190M total / 41M recent; last release 2026-05-18. Rust-lang org, stable. |
| zstd compression | **`zstd`** | 0.13.3 | Thin, fast binding to libzstd; `stream::Encoder`/`Decoder` compose as plain `Write`/`Read` wrappers. | 328M total / 65M recent; last release 2025-02-20. Very mature. |
| Encryption | **`age`** | 0.12.1 | Reference Rust impl of the age format; passphrase (scrypt) whole-stream encryption via `wrap_output`, no key management. | 2.7M total / 1.3M recent; last release 2026-07-14. Maintained by str4d (spec author); self-labelled BETA. |
| Filesystem walking | **`walkdir`** | 2.5.0 | Minimal recursive walker; simplest thing that works for "collect a category's paths". | 514M total / 120M recent; last release 2024-03-01. BurntSushi, rock-stable. |
| Manifest (de)serialization | **`serde`** | 1.0.228 | Ecosystem-standard derive-based serialization framework. | 1.16B total / 233M recent; last release 2025-09-27. |
| Manifest JSON | **`serde_json`** | 1.0.150 | Canonical JSON backend for serde; `to_writer_pretty` straight into the sidecar file. | 1.06B total / 231M recent; last release 2026-05-21. |

### Runners-up (deliberately not chosen for the alpha)

| Concern | Alternative | Version | Why not now (but keep in mind) |
|---|---|---|---|
| TUI | `ratatui` | 0.30.2 | Full-screen TUI framework (37.7M total / 14M recent, released 2026-06-19). Best-in-class UX, but you build the event loop, widgets, and state yourself — large build cost for three prompts. Adopt later if the tool grows a persistent dashboard. |
| Prompts | `dialoguer` | 0.12.0 | Also has `MultiSelect`/`Password` and is heavily used (69.9M total / 15M recent, 2025-08-23, console-rs). Equally valid; `inquire` edges it on validation, filtering, and defaults ergonomics. |
| FS walking | `ignore` | 0.4.28 | Adds `.gitignore`-style exclude rules + parallel walking (145M total / 31M recent, 2026-07-09, BurntSushi/ripgrep). Upgrade target once you want to skip caches/`node_modules` via ignore patterns; overkill for MVP. |
| Terminal backend | `crossterm` | 0.29.0 | Not a direct dependency — `inquire` (and `ratatui`) pull it in transitively. Only depend on it directly if you drop to raw terminal control. |

**Net alpha dependency line (Cargo.toml):**

```toml
inquire    = "0.9"
tar        = "0.4"
zstd       = "0.13"
age        = { version = "0.12", features = ["armor"] }  # armor optional; see note
walkdir    = "2"
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
secrecy    = "0.8"   # for age's SecretString passphrase wrapper
```

## How tar → zstd → age compose (streaming pipeline)

Compress **before** you encrypt (ciphertext is incompressible), so the layering, from
innermost payload to outermost bytes-on-disk, is:

```
files ─► tar (Builder)  ─►  zstd (Encoder)  ─►  age (StreamWriter)  ─►  output .tar.zst[.age]
         innermost           middle              outermost
```

Each layer is just a `Write` wrapping the next writer, so you build them **outermost-first**
in code and finish them **innermost-first**:

```rust
let file = File::create(&out_path)?;                       // sink
let encryptor = age::Encryptor::with_user_passphrase(pass); // scrypt recipient
let age_w  = encryptor.wrap_output(file)?;                  // outermost: encrypts
let zstd_w = zstd::stream::Encoder::new(age_w, 19)?;        // middle: compresses
let mut tar = tar::Builder::new(zstd_w);                    // innermost: frames files

tar.append_dir_all(".", &root)?;                            // walkdir feeds the paths
let zstd_w = tar.into_inner()?;   // finish tar
let age_w  = zstd_w.finish()?;    // finish zstd (flush frame)
age_w.finish()?;                  // finish age (write auth tag) — MUST call
```

Restore reverses the wrap order: `File → age::Decryptor → zstd::Decoder → tar::Archive`.
The whole thing is single-pass streaming — no temp files, constant memory.
The `manifest.json` is written **separately** via `serde_json::to_writer_pretty` (it is a
plaintext sidecar describing what/when/categories, not part of the encrypted stream).

## Friction call-outs

- **tar ownership / xattrs are not automatic.**
  - *Creation:* the `tar` crate preserves file **mode** and stores symlinks as symlinks
    (`Builder::follow_symlinks(false)`), but it does **not** harvest extended attributes
    (xattrs / POSIX ACLs / file capabilities) on its own — you'd have to add PAX-header
    entries yourself. For an alpha, document that xattrs/ACLs are out of scope.
  - *Restore:* `Archive` gates preservation behind explicit setters —
    `set_preserve_permissions(true)`, `set_preserve_ownerships(true)`,
    `set_unpack_xattrs(true)` (all confirmed present in 0.4.46). Restoring **uid/gid**
    only works when running as **root**; as a normal user, ownership silently falls back
    to the current user. Note this in the restore UX.

- **`age` API ergonomics.**
  - `Encryptor::with_user_passphrase(...)` takes a `secrecy::SecretString`, so you either
    add the `secrecy` crate or use age's re-export — a small papercut for a "just a
    passphrase" flow.
  - `wrap_output()` returns a `StreamWriter` that **must** be `.finish()`ed, or the
    authentication tag is never written and the archive won't decrypt. Easy to forget
    behind the zstd/tar layers.
  - Decryption uses the scrypt identity (`age::scrypt::Identity`) fed to
    `Decryptor::decrypt(...)`; the decryptor API has shifted across 0.9→0.10+ releases, so
    pin the version and follow the 0.12 docs rather than older blog snippets.
  - age is self-labelled **BETA** and the wire format is stable/spec-backed, but expect
    minor API churn between minor versions — worth a version pin.

- **zstd `Encoder` must be finished too** (`.finish()` / `auto_finish()`), or the last
  frame is truncated. Compression level ~3–7 is a saner alpha default than 19 for speed.

## Sources
- crates.io API (`/api/v1/crates/<name>`), fetched 2026-07-14.
- docs.rs — `age` 0.12.1 (`struct.Encryptor.html`), `tar` 0.4.46 (`struct.Archive.html`).
- `cargo search` for latest published versions.
