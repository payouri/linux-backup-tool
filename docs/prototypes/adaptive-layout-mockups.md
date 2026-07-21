# PROTOTYPE — adaptive layout & per-screen design (wayfinder #50)

> **Throwaway artifact.** ASCII mockups to react to, not code to keep. Lives on branch
> `prototype/adaptive-layout`. The *decisions* it settles get folded into the implementation
> spec (#53); this file itself is captured as a primary source and does not land on `trunk`.
>
> **Question (#50):** How does each screen look and reflow across terminal sizes so it never
> feels cluttered or cramped?

Medium note: a ratatui screen **is** a monospace character grid, so these ASCII frames are
1:1 with what the terminal actually draws — not an approximation. Frames are 78 wide (an
80-col terminal minus the two border columns).

**Revised after review** — decisions locked in this round are marked ✅ inline.

---

## 0. What is already fixed (do not re-decide here)

From **ADR-0005** and **ADR-0007** — the mockups obey these; #50 only fills the visual gaps:

- Fixed **header** (stepper) + swapping **body** + fixed **footer** (keymap hints). Header and
  footer do **not** reflow-jump when the step changes; only the body region swaps. (ADR-0007 D6)
- Horizontal stepper in the header, 4 steps, three states — **completed** (jumpable),
  **current**, **locked/ahead** (dimmed). (ADR-0007 D7)
- Footer shows available moves; forward gate reads **disabled-with-reason** when incomplete.
- List viewport: **scroll-on-margin, clamp at ends, no wrap.** (ADR-0007 D6)
- **Redraw-on-event** (one draw per state change), except RUN which polls the progress channel.

---

## 1. Chrome anatomy & glyphs

✅ **No app name in the chrome** — the stepper already says where you are; the top border is the
stepper itself (wizard screens) or a short mode label (RUN/restore).

```
┌─ ✓ WHAT  →  ● Encryption  →  · WHERE  →  · Review ──────────────────────────┐  header: stepper only
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  <body — the only region that swaps between steps>                          │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → drill · Ctrl-N next · ? keys                     │  footer: keymap hints
└─────────────────────────────────────────────────────────────────────────────┘
```

✅ **Stepper states** — order is conveyed by *position*, so no numbering. The **current step is
marked by color/contrast on its title** (bold + accent, not just a glyph); other states use a
small leading glyph:

| state           | glyph | title style             | selectable |
|-----------------|-------|-------------------------|------------|
| completed       | `✓`   | normal                  | yes (jump) |
| **current**     | `●`   | **bold + accent color** | —          |
| locked / ahead  | `·`   | dim gray                | no         |
| invalidated     | `!`   | yellow                  | yes (jump) |

Invalidated `!` is the ADR-0007 D5 case: a completed Encryption step dropped back to incomplete
because a WHAT edit newly exposed Sensitive content in plaintext; the frontier collapses to it.
Separator is ` → `. A Sensitive selection gives the Encryption label a lock: `● Encryption 🔒`
(`[!]` in a no-emoji terminal — §7).

---

## 2. The reflow ladder (adaptive height)

The screen degrades in a fixed order as rows shrink. **Body always wins**; chrome sheds first.

| tier            | rows   | header                                        | footer                       |
|-----------------|--------|-----------------------------------------------|------------------------------|
| **Comfortable** | ≥ 30   | stepper row + a context/status line under it  | full hint line               |
| **Default**     | 20–29  | stepper row only                              | full hint line               |
| **Tight**       | 14–19  | stepper compacts (glyphs + current title only)| essential hints + `· ? keys` |
| **Minimum**     | 10–13  | 1-line compact stepper                        | `Ctrl-N next · ? keys`        |
| **Refuse**      | < 10   | full-screen notice: "Terminal too short — needs ≥ 10 rows." |

Rules:
- The **body never drops below 6 usable rows**; chrome sheds to its Minimum form before the body
  is squeezed.
- The **context/status line** (e.g. "388 selected · 1.24 GB · ↺ restored · 0 pruned") exists only
  at Comfortable; below that it folds into REVIEW and the footer.
- Scroll indicator column appears inside a list only when it overflows.

*(Height-reflow of the two-pane WHAT screen is shown across §3's three sizes.)*

---

## 3. WHAT + drill-down — two-pane explorer ✅ (Variant B, chosen)

✅ **Left column = Categories** (with a **selected count**). **Right column = a single-directory
explorer** showing only the *current directory's* contents — **not** an indented tree. The
**explored path sits above the explorer column** as a breadcrumb. **Focus toggles between the two
columns at any time** (`Tab`); the focused column carries a brighter border + the row cursor.

Descend by pressing `→`/`Enter` on a directory in the explorer (the breadcrumb grows, the column
reloads with that directory's children); `←`/`Backspace` climbs one level. Toggling a Category on
the left, or a node on the right, sets selection; tri-state is derived per ADR-0006.

✅ **Counts**: left rows show **selected Entry count**. Explorer **directory** rows show their
**child count**; an **empty directory shows `—` and is drill-disabled** (dimmed, `→` no-ops).
File rows show a size. No running "kept" arithmetic.

**Comfortable — 80×32** (focus on the right/explorer column)
```
┌─ ● WHAT  →  · Encryption  →  · WHERE  →  · Review ──────────────────────────┐
│ 388 selected · 1.24 GB · ↺ restored last selection · 0 pruned                │
├──────────────────────────┬──────────────────────────────────────────────────┤
│ Categories               │ KDE Plasma ▸ .local/share ▸ plasma               │
│                          │                                                  │
│ [x] SSH            3  🔒 │ [x] plasmoids/                        30 items   │
│ [x] KDE Plasma   388     │ [ ] wallpapers/                        6 items   │
│ [x] Shell          9     │ [~] look-and-feel/                     4 items   │
│ [ ] Program cfg   64     │ [x] plasmashellrc                        12 KB   │
│ [x] Package list   6     │ [x] plasmarc                              3 KB   │
│ [x] GPG            5  🔒 │ [ ] .cache-marker/                           —   │
│ [+] Custom paths   0     │                                                  │
│                          │                                                  │
│                          │                                                  │
├──────────────────────────┴──────────────────────────────────────────────────┤
│ Tab focus panes · ↑↓ move · Space toggle · → into dir · ← up · Ctrl-N next · ?│
└─────────────────────────────────────────────────────────────────────────────┘
```
Reading the rows: `[x]` selected · `[~]` Partial · `[ ]` deselected (a `[ ]` directory is a
subtree deselection); a directory's metric is its **child count**, a file's is its **size**; a
`—` metric means empty → the row is dimmed and drill-disabled. Left metric is the **selected
Entry count** per Category.

**Default — 80×24** (context line gone; focus on the left/Categories column — note the cursor)
```
┌─ ● WHAT  →  · Encryption  →  · WHERE  →  · Review ──────────────────────────┐
├──────────────────────────┬──────────────────────────────────────────────────┤
│ Categories               │ KDE Plasma ▸ .local/share                         │
│                          │                                                  │
│▸[x] KDE Plasma   388     │ [x] color-schemes/                6 items         │
│ [x] SSH            3  🔒 │ [~] plasma/                       41 items        │
│ [x] Shell          9     │ [ ] icons/                        22 items        │
│ [ ] Program cfg   64     │ [x] konsole/                       4 items        │
│ [x] Package list   6     │ [x] user-places.xbel               8 KB           │
│ [x] GPG            5  🔒 │ [ ] .cache-marker/                 —              │
│ [+] Custom paths   0     │                                                  │
├──────────────────────────┴──────────────────────────────────────────────────┤
│ Tab focus · ↑↓ move · Space toggle · → into dir · Ctrl-N next · ? keys       │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Tight — 80×16** (compact stepper; both columns scroll independently on overflow)
```
┌ ✓ · [●WHAT] · → ─────────────────────────────────────────────────────────── ┐
├──────────────────────────┬──────────────────────────────────────────────────┤
│ [x] KDE Plasma   388     │ KDE Plasma ▸ .local/share                         │
│ [x] SSH            3  🔒 │ [x] color-schemes/                6 items    ▲     │
│ [x] Shell          9     │ [~] plasma/                       41 items   █     │
│ [ ] Program cfg   64     │ [ ] icons/                        22 items   │     │
│ [x] GPG            5  🔒 │ [x] konsole/                       4 items    ▼     │
├──────────────────────────┴──────────────────────────────────────────────────┤
│ Tab · ↑↓ · Space · → dir · Ctrl-N next · ? keys                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Narrow width — < ~70 cols** ✅ the split can't hold, so it collapses to a **single active
column**: whichever pane has focus fills the width; `Tab` swaps which one is shown; the breadcrumb
stays pinned above when the explorer is active.
```
┌ ●WHAT → · Enc → · WHERE → · Review ──────────┐
├──────────────────────────────────────────────┤
│ KDE Plasma ▸ .local/share                    │
│ [x] color-schemes/            6 items        │
│ [~] plasma/                  41 items        │
│ [ ] icons/                   22 items       │
│ [x] konsole/                  4 items        │
├──────────────────────────────────────────────┤
│ Tab → Categories · ↑↓ · Space · → dir · ?    │
└──────────────────────────────────────────────┘
```

---

## 4. Encryption screen

Two behaviors (ADR-0007 D4): **required** (Sensitive → Auto-insist) vs **offered** (non-Sensitive,
opt-in). Masked field + reveal (chord owned by #51; the *field* is here).

**Required — Sensitive selection (Auto-insist ON)**
```
┌─ ✓ WHAT  →  ● Encryption 🔒  →  · WHERE  →  · Review ───────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  Encryption                                                                  │
│                                                                             │
│  This selection contains Sensitive content, so the whole Archive will be     │
│  encrypted. Sensitive Categories: SSH, GPG                                   │
│                                                                             │
│  Passphrase        [ •••••••••••••          ]  (F2 reveal)                   │
│  Confirm           [ •••••••••••••          ]                                │
│                    ✓ passphrases match                                       │
│                                                                             │
│  Or: [ disable encryption (plaintext) ] — asks a second time, naming what    │
│      lands in cleartext.                                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│ type passphrase · F2 reveal · Ctrl-N next · ? keys                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Offered — non-Sensitive selection (opt-in)**
```
│  Encryption (optional)                                                       │
│  Nothing in this selection is Sensitive. Leave it plaintext, or set a        │
│  passphrase to encrypt it anyway.                                            │
│                                                                             │
│  ( ) Plaintext  (default)                                                    │
│  (•) Encrypt with a passphrase                                              │
│      Passphrase [ ••••••••         ]  (F2 reveal)                            │
│      Confirm    [ ••••••••         ]   ✓ match                               │
```

**Reveal** flips the field in place: `[ correct-horse-battery ]  (F2 hide)`.

**Second confirmation** (disable Auto-insist) — modal over the body, naming Categories:
```
│  ┌ Ship credentials in PLAINTEXT? ───────────────────────────────────────┐  │
│  │ These Sensitive Categories will be written unencrypted:  • SSH  • GPG  │  │
│  │ Type  PLAINTEXT  to confirm:  [ ______ ]        [ Cancel ]  [ Confirm ]│  │
│  └───────────────────────────────────────────────────────────────────────┘  │
```

---

## 5. WHERE, REVIEW, RUN, restore, passphrase-entry

**WHERE** — locations/drives + free-space; cheap-validate (ADR-0007 D2).
```
┌─ ✓ WHAT  →  ✓ Encryption  →  ● WHERE  →  · Review ─────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  WHERE to write the backup                       Selection: 1.24 GB          │
│                                                                             │
│  (•) $HOME                       /home/youri            412 GB free          │
│  ( ) USB                         /run/media/youri/USB   3.1 GB free  ⚠ tight │
│  ( ) Type a path…                [ /mnt/backups________________ ]  ✓ writable│
│                                                                             │
│  ⚠ 1.24 GB selection is close to USB's 3.1 GB free space.                    │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ choose · Enter pick · type for custom · Ctrl-N next · ? keys              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**REVIEW** — the `review_text` summary; terminal step (complete = launch, ADR-0007 D2).
✅ **Back-jump via the header stepper only** — no duplicate inline buttons.
```
┌─ ✓ WHAT  →  ✓ Encryption  →  ✓ WHERE  →  ● Review ─────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  REVIEW — nothing is written until you confirm                               │
│                                                                             │
│  Selection   128 Entries · 4 Categories · 1.24 GB                            │
│     SSH 3 · KDE Plasma 388 · Shell 9 · GPG 5                                 │
│  Output      /run/media/youri/USB                                            │
│  Backup dir  …/USB/linux-backup-thinkpad-20260721-1530                       │
│  Encryption  encrypted with a passphrase (Archive + Manifest .age)           │
│                                                                             │
│  (To change anything, jump back via a ✓ step in the header.)                 │
├─────────────────────────────────────────────────────────────────────────────┤
│ Ctrl-Enter start backup · ← back · ? keys                                    │
└─────────────────────────────────────────────────────────────────────────────┘
```

**RUN** — folds `indicatif` into the frame (ADR-0005 D4, channel-fed). No stepper (out of the
wizard); a short mode label instead.
```
┌─ running ──────────────────────────────────────────────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  Backing up…                                                                 │
│                                                                             │
│  Writing Archive                                                             │
│  [=========================>                    ]  216 / 412 Entries         │
│                                                                             │
│  ⠸  .local/share/plasma/plasmoids/org.kde.plasma.systemtray/main.qml         │
│                                                                             │
│  Phases:  ✓ Reading   ● Writing Archive   · Writing Manifest                 │
├─────────────────────────────────────────────────────────────────────────────┤
│ Ctrl-C cancel (removes the partial Backup dir — fail-fast, ADR-0002)         │
└─────────────────────────────────────────────────────────────────────────────┘
```
Done swaps the body to `success_summary` (dir, entries, encryption, Vanished call-outs, package
counts); footer → `Enter close`.

**Restore overwrite-confirm** — best-effort framing (ADR-0002).
```
┌─ restore ──────────────────────────────────────────────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  Restore from  …/USB/linux-backup-thinkpad-20260721-1530                     │
│                                                                             │
│  Writes 128 Entries back under /home/youri, overwriting existing files.      │
│  Restore is best-effort: per-file failures are reported, not fatal.          │
│  Encrypted backup — passphrase required.                                     │
│                                                                             │
│                              [ Cancel ]        [ Restore ]                   │
├─────────────────────────────────────────────────────────────────────────────┤
│ ←→ choose · Enter confirm · Esc cancel · ? keys                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Passphrase entry** (restore / decrypt) — single masked field, reveal, retry-on-wrong.
```
│  Enter passphrase to decrypt this backup                                     │
│  [ ••••••••••••          ]  (F2 reveal)                                      │
│  ✗ wrong passphrase — try again (attempt 2)                                  │
```

---

## 6. Empty / edge states

- **Hide-empty Category** — simply absent from the left column (Custom paths is the exempt
  always-shown add affordance).
- **Empty Custom paths** — `[+] Custom paths   0`; `→` opens the autocomplete field (#52).
- **Empty directory** in the explorer — child count `—`, row dimmed, **drill-disabled** (`→`
  no-ops). The `—` and dim carry it; no `(empty)` label. ✅
- **Subtree deselected** — the `[ ]` unchecked state carries it; no `(off)` label.
- **Long paths** — middle-truncate with `…` (breadcrumb and Backup dir); never wrap a path.
- **Many entries** — column scrolls (scroll-on-margin); indicator column on overflow only.
- **Empty selection** — WHAT forward gate disabled: `Ctrl-N next — select at least one Category`.
- **Invalidated encryption** — stepper `! Encryption`, frontier collapses to it, banner in the
  encryption body: `Selection changed — new Sensitive content needs a decision.`

---

## 7. No-emoji / limited terminal fallback

`🔒` → `[!]`, spinner `⠸` → `|/-\`, checkbox glyphs stay ASCII (`[x] [~] [ ] [+]`), stepper
`✓ ● · !` → `[x] > . !`. Detected once at startup; one look, no runtime toggle (theming is out
of scope per the map).

---

## Decisions locked this round ✅

1. **WHAT/drill = two-pane**: left Categories (selected count), right **single-directory explorer**
   (current dir only, not a tree), breadcrumb above the explorer, **focus toggles freely** (`Tab`).
   Collapses to one active column below ~70 cols.
2. **No app name** anywhere in the chrome.
3. **Stepper**: symbol glyphs, order by position (no numbers), **current step marked by
   color/contrast on its title**.
4. **Counts**: left = selected count; explorer directories = child count, `—` when empty; **empty
   dirs drill-disabled**. No running "kept" math.
5. **REVIEW** back-jump via the header stepper only.

Still deferred (map's *Not yet specified*): exact Ctrl/Alt chords (#51) and per-widget error/empty
copy graduate once keymap + this layout resolve.
```
