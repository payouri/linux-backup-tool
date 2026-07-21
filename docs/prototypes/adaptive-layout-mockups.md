# PROTOTYPE — adaptive layout & per-screen design (wayfinder #50)

> **Throwaway artifact.** ASCII mockups to react to, not code to keep. Lives on branch
> `prototype/adaptive-layout`. The *decisions* it settles get folded into the implementation
> spec (#53); this file itself is captured as a primary source and does not land on `trunk`.
>
> **Question (#50):** How does each screen look and reflow across terminal sizes so it never
> feels cluttered or cramped?

Medium note: a ratatui screen **is** a monospace character grid, so these ASCII frames are
1:1 with what the terminal actually draws — not an approximation. Column ruler is 78 wide
(an 80-col terminal minus the two border columns).

---

## 0. What is already fixed (do not re-decide here)

From **ADR-0005** and **ADR-0007** — the mockups obey these; #50 only fills the visual gaps:

- Fixed **header** (stepper) + swapping **body** + fixed **footer** (keymap hints). Header and
  footer do **not** reflow-jump when the step changes; only the body region swaps. (ADR-0007 D6)
- Horizontal stepper in the header showing the 4 wizard steps in three states —
  **completed** (jumpable), **current**, **locked/ahead** (dimmed). (ADR-0007 D7)
- Footer shows available moves; forward gate reads **disabled-with-reason** when the step is
  incomplete. (ADR-0007 D7)
- List viewport: **scroll-on-margin, clamp at ends, no wrap.** (ADR-0007 D6)
- **Redraw-on-event** (one draw per state change), except RUN which polls the progress channel.

What #50 decides: concrete glyphs/colors, the **reflow ladder** (what collapses as height
shrinks), per-screen body layout, information density, and empty/edge states.

---

## 1. Chrome anatomy & glyphs

```
┌─ linux-backup · backup ────────────────────────────────────────────────────┐  header row 1: app + command
│ ✓ WHAT  →  ● Encryption  →  · WHERE  →  · Review                             │  header row 2: stepper
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  <body — the only region that swaps between steps>                          │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → drill · Ctrl-N next · ? keys                     │  footer: keymap hints
└─────────────────────────────────────────────────────────────────────────────┘
```

**Stepper glyph states** (chord assignments themselves belong to #51; these are visual states):

| state              | glyph | style             | selectable |
|--------------------|-------|-------------------|------------|
| completed          | `✓`   | green             | yes (jump) |
| current            | `●`   | bold / reversed   | —          |
| locked / ahead     | `·`   | dim gray          | no         |
| **invalidated**    | `!`   | yellow            | yes (jump) |

The `!` state is the ADR-0007 D5 case: a completed encryption step dropped back to incomplete
because a WHAT edit newly exposed Sensitive content in plaintext. It reads as "needs attention"
and the frontier collapses back to it.

Separator between steps is ` → `. On a Sensitive selection the Encryption label gains a lock:
`● Encryption 🔒` (or `[!]` in a no-emoji terminal — see §7 fallback).

---

## 2. The reflow ladder (adaptive height)

The screen degrades in a fixed order as rows shrink. **Body always wins**; chrome sheds first.

| tier          | rows   | header                              | footer                          |
|---------------|--------|-------------------------------------|---------------------------------|
| **Comfortable** | ≥ 30 | title row + stepper row (2 lines) + a context/status line under it | full hint line |
| **Default**   | 20–29  | title folded into stepper row (1 line) | full hint line |
| **Tight**     | 14–19  | stepper compacts to `[2/4] Encryption` | essential hints only + `· ? keys` |
| **Minimum**   | 10–13  | 1-line compact stepper              | 1-line, `Ctrl-N next · ? keys`  |
| **Refuse**    | < 10   | full-screen notice: "Terminal too short — needs ≥ 10 rows." |

Rules:
- The **body never drops below 6 usable rows**; if chrome + body can't both fit, chrome sheds
  to its Minimum form before the body is squeezed.
- Lists inside the body scroll (scroll-on-margin); the body itself never introduces a second
  scrollbar for chrome.
- A **status/context line** (e.g. "3 Categories · 128 Entries · 1.2 GB") exists only at
  Comfortable; at Default and below it folds into the REVIEW step and the footer.

### Same screen (WHAT), three heights

**Comfortable — 80×32**
```
┌─ linux-backup · backup ────────────────────────────────────────────────────┐
│ ● WHAT  →  · Encryption  →  · WHERE  →  · Review                             │
│ 4 Categories on · 128 Entries · 1.24 GB · ↺ restored last selection · 0 pruned│
├─────────────────────────────────────────────────────────────────────────────┤
│  WHAT to back up                                                             │
│                                                                             │
│  [x] SSH                       3 Entries      🔒 sensitive        →          │
│  [x] KDE Plasma              412 Entries         drill to trim    →          │
│  [x] Shell                     9 Entries                          →          │
│  [ ] Program configs          64 Entries                         →          │
│  [x] Package lists             6 managers                         →          │
│  [ ] User data              (default off — bulk data, size warning)          │
│  [x] GPG                       5 Entries      🔒 sensitive        →          │
│  [ ] Cloud credentials       (empty on this machine — hidden by Hide-empty) │
│  [+] Custom paths              0 added        press → to add                 │
│                                                                             │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → drill/add · Ctrl-N next · ? keys                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Default — 80×24** (title folds into stepper row; context line gone)
```
┌─ backup ── ● WHAT → · Encryption → · WHERE → · Review ──────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  WHAT to back up                                              128 Entries · 1.2 GB │
│                                                                             │
│  [x] SSH                       3 Entries      🔒 sensitive        →          │
│  [x] KDE Plasma              412 Entries         drill to trim    →          │
│  [x] Shell                     9 Entries                          →          │
│  [ ] Program configs          64 Entries                         →          │
│  [x] Package lists             6 managers                         →          │
│  [x] GPG                       5 Entries      🔒 sensitive        →          │
│  [+] Custom paths              0 added        press → to add                 │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → drill/add · Ctrl-N next · ? keys                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Tight — 80×16** (compact stepper `[1/4]`; list scrolls; essential hints)
```
┌ [1/4] WHAT ─────────────────────────────────────────────── 128 Entries · 1.2 GB ┐
├─────────────────────────────────────────────────────────────────────────────┤
│  [x] SSH                       3 Entries      🔒 sensitive        →          │
│  [x] KDE Plasma              412 Entries         drill to trim    →   ▲      │
│  [x] Shell                     9 Entries                          →   █      │
│  [ ] Program configs          64 Entries                         →   █      │
│  [x] Package lists             6 managers                         →   │      │
│  [x] GPG                       5 Entries      🔒 sensitive        →   ▼      │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ · Space · → drill · Ctrl-N next · ? keys                                  │
└─────────────────────────────────────────────────────────────────────────────┘
```
(The `▲ █ │ ▼` column on the right is the scroll indicator — shown only when the list overflows.)

---

## 3. WHAT + drill-down — THE BIG FORK

Two genuinely different shapes. This is the main thing to pick.

### Variant A — single full-screen list; drill *replaces* the body

WHAT is one list (as in §2). Pressing `→` on a Category swaps the whole body to the recursive
tree for that Category, with a breadcrumb. `←` / `Esc` pops back up a level, then back to WHAT.

Drill-down tree (recursive, tri-state per ADR-0006: All `[x]` / Partial `[~]` / None `[ ]`):
```
┌─ backup ── ● WHAT → · Encryption → · WHERE → · Review ──────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  KDE Plasma ▸ .local/share                        412 → 388 Entries kept     │  breadcrumb + running count
│                                                                             │
│  [x] .config/                                     198 Entries               │
│  [~] .local/share/                                214 → 190 Entries    ▾     │  Partial, expanded
│      [x] color-schemes/                             6 Entries               │
│      [ ] icons/                                     22 Entries  (subtree off)│  Subtree deselection
│      [~] plasma/                                    41 → 35 Entries    ▾     │
│          [x] plasmoids/                             30 Entries              │
│          [ ] wallpapers/                             6 Entries  (subtree off)│
│      [x] konsole/                                    4 Entries              │
│  [x] .local/state/                                  0 Entries               │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → expand/descend · ← up · Esc back to WHAT · ? keys │
└─────────────────────────────────────────────────────────────────────────────┘
```
- `[~]` = Partial selection (derived `directory_state → Partial`, ADR-0006).
- "(subtree off)" marks a `deselected_roots` prefix cut. Prefix-wins → children show but are
  visibly withheld, no per-child re-include (ADR-0006 D2).
- Running `412 → 388 kept` count updates live so the cost of a cut is visible.

**Pros:** every screen is one full-width list; simple mental model; scales to any depth without
running out of horizontal room; least code. **Cons:** you lose sight of the Category list while
drilling; deep trees need breadcrumb reading.

### Variant B — two-pane master/detail

Categories pinned left (~26 cols); the right pane shows the selected Category's tree. `→` moves
focus into the tree; `←` back to the list. Nothing swaps — you always see both.
```
┌─ backup ── ● WHAT → · Encryption → · WHERE → · Review ──────────────────────┐
├──────────────────────────┬──────────────────────────────────────────────────┤
│ [x] SSH          3        │ KDE Plasma ▸ .local/share        412 → 388 kept   │
│ [x] KDE Plasma   412  →   │                                                  │
│ [x] Shell        9        │ [x] .config/                     198 Entries      │
│ [ ] Program cfg  64       │ [~] .local/share/                214 → 190   ▾    │
│ [x] Package list 6        │     [x] color-schemes/             6              │
│ [x] GPG          5   🔒   │     [ ] icons/                     22 (off)       │
│ [+] Custom paths 0        │     [~] plasma/                    41 → 35    ▾    │
│                          │         [x] plasmoids/             30             │
│                          │         [ ] wallpapers/             6 (off)       │
│                          │     [x] konsole/                    4              │
├──────────────────────────┴──────────────────────────────────────────────────┤
│ ↑↓ move · Space toggle · → into tree · ← to categories · Ctrl-N next · ? keys│
└─────────────────────────────────────────────────────────────────────────────┘
```
**Pros:** category context never lost; toggling a Category and seeing its tree is one glance;
feels like a file manager. **Cons:** ~26 cols eaten → deep trees wrap/truncate on narrow
terminals; below ~70 cols the split must collapse to Variant A anyway (so B implies building A
as its narrow fallback); more layout code.

> **Decision needed:** A or B as the primary WHAT/drill shape? (Recommendation: **A** — it's the
> minimum-viable shape, degrades cleanly to any width, and ADR-0006 already made the tree a
> derived view so a full-screen tree is the least code. B can be a later enhancement if the
> two-pane glance proves worth the width cost.)

---

## 4. Encryption screen

Two behaviors (ADR-0007 D4): **required** (Sensitive selection, Auto-insist) vs **offered**
(non-Sensitive, opt-in). Masked passphrase field with reveal toggle (#51 owns the chord; the
*field* is here).

**Required — Sensitive selection (Auto-insist ON)**
```
┌─ backup ── ✓ WHAT → ● Encryption 🔒 → · WHERE → · Review ───────────────────┐
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
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ type passphrase · F2 reveal · Ctrl-N next · ? keys                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Offered — non-Sensitive selection (opt-in)**
```
│  Encryption (optional)                                                       │
│                                                                             │
│  Nothing in this selection is Sensitive. You can leave the backup as        │
│  plaintext, or set a passphrase to encrypt it anyway.                        │
│                                                                             │
│  ( ) Plaintext  (default)                                                    │
│  (•) Encrypt with a passphrase                                              │
│      Passphrase [ ••••••••         ]  (F2 reveal)                            │
│      Confirm    [ ••••••••         ]   ✓ match                               │
```

**Reveal toggle** flips the masked field to cleartext in place:
```
│  Passphrase        [ correct-horse-battery ]  (F2 hide)                      │
```

**Second confirmation** (disable Auto-insist) — a modal over the body, naming Categories:
```
│  ┌ Ship credentials in PLAINTEXT? ───────────────────────────────────────┐  │
│  │ These Sensitive Categories will be written unencrypted:               │  │
│  │   • SSH   • GPG                                                        │  │
│  │ Type  PLAINTEXT  to confirm:  [ ______ ]                               │  │
│  │                                    [ Cancel ]   [ Confirm ]           │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
```

---

## 5. WHERE, REVIEW, RUN, restore, passphrase-entry

**WHERE** — locations/drives with free-space; cheap-validate (ADR-0007 D2).
```
┌─ backup ── ✓ WHAT → ✓ Encryption → ● WHERE → · Review ─────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  WHERE to write the backup                       Selection: 1.24 GB          │
│                                                                             │
│  (•) $HOME                       /home/youri            412 GB free          │
│  ( ) USB                         /run/media/youri/USB   3.1 GB free  ⚠ tight │
│  ( ) Type a path…                [ /mnt/backups________________ ]  ✓ writable│
│                                                                             │
│  ⚠ 1.24 GB selection is close to USB's 3.1 GB free space.                    │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ choose · Enter pick · type for custom · Ctrl-N next · ? keys              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**REVIEW** — the summary from `review_text`; terminal step, "complete = launch" (ADR-0007 D2).
```
┌─ backup ── ✓ WHAT → ✓ Encryption → ✓ WHERE → ● Review ─────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  REVIEW — nothing is written until you confirm                               │
│                                                                             │
│  Selection   128 Entries · 4 Categories · 1.24 GB                            │
│     SSH 3 · KDE Plasma 388 · Shell 9 · GPG 5                                 │
│  Output      /run/media/youri/USB                                            │
│  Backup dir  …/USB/linux-backup-thinkpad-20260721-1530                       │
│  Encryption  encrypted with a passphrase (Archive + Manifest .age)           │
│                                                                             │
│  Jump back:  [WHAT]  [Encryption]  [WHERE]   (all completed → jumpable)      │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ Ctrl-Enter start backup · ← back · jump: g then step · ? keys               │
└─────────────────────────────────────────────────────────────────────────────┘
```

**RUN** — folds `indicatif` into the frame (ADR-0005 D4, channel-fed). The only screen that
polls; no stepper (we've left the wizard).
```
┌─ linux-backup · running ───────────────────────────────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  Backing up…                                                                 │
│                                                                             │
│  Writing Archive                                                             │
│  [=========================>                    ]  216 / 412 Entries         │
│                                                                             │
│  ⠸  .local/share/plasma/plasmoids/org.kde.plasma.systemtray/main.qml         │  current entry
│                                                                             │
│  Phases:  ✓ Reading   ● Writing Archive   · Writing Manifest                 │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ Ctrl-C cancel (removes the partial Backup dir — fail-fast, ADR-0002)         │
└─────────────────────────────────────────────────────────────────────────────┘
```
Done state swaps the body to the `success_summary` text (Backup dir, entries, encryption,
Vanished call-outs, package counts) and the footer to `Enter close`.

**Restore overwrite-confirm** — modal-style, best-effort framing (ADR-0002).
```
┌─ linux-backup · restore ───────────────────────────────────────────────────┐
├─────────────────────────────────────────────────────────────────────────────┤
│  Restore from  …/USB/linux-backup-thinkpad-20260721-1530                     │
│                                                                             │
│  This will write 128 Entries back under /home/youri, overwriting existing    │
│  files. Restore is best-effort: per-file failures are reported, not fatal.   │
│                                                                             │
│  Encrypted backup — passphrase required (see passphrase entry).             │
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

- **Hide-empty Category** — never listed (Cloud credentials line in §2 shown only to illustrate;
  in the real UI it's simply absent). Custom paths is the exempt always-shown add affordance.
- **Empty Custom paths** — `[+] Custom paths   0 added   press → to add`; drilling opens the
  autocomplete field (#52).
- **Long paths** — middle-truncate with `…`: `…/USB/linux-backup-thinkpad-20260721-1530`. Never
  wrap a path across two rows in a list.
- **Many entries** — list scrolls (scroll-on-margin); scroll indicator column appears only on
  overflow (see Tight mockup §2).
- **Empty selection** — WHAT forward gate disabled: footer reads
  `Ctrl-N next — select at least one Category first` (ADR-0007 D2 gate, disabled-with-reason).
- **Invalidated encryption** — stepper shows `! Encryption`, frontier collapses back to it, a
  banner in the encryption body: `Selection changed — new Sensitive content needs a decision.`

---

## 7. No-emoji / limited terminal fallback

`🔒` → `[!]`, spinner `⠸` → `|/-\`, checkbox glyphs stay ASCII (`[x] [~] [ ] [+]`), stepper
`✓ ● · !` → `[x] > . !`. Detected once at startup; one look, no runtime toggle (theming is
out of scope per the map).

---

## Open decisions for the human

1. **WHAT/drill shape: Variant A (single full-screen) or B (two-pane)?** (Rec: A.)
2. **Reflow ladder tiers** — are the row thresholds (30 / 20 / 14 / 10) and the shed order
   (title → context line → hint verbosity → stepper compaction) right?
3. **Stepper glyphs** — `✓ ● · !` with green/bold/dim/yellow — good, or prefer numbered
   `① ② ③ ④`?
4. **Running "kept" count** in drill-down (`412 → 388 kept`) — keep it, or too noisy?
5. **REVIEW jump affordance** — inline `[WHAT] [Encryption] [WHERE]` buttons, or rely only on
   the header stepper being jumpable?
