# PROTOTYPE — fixed cross-screen keymap (wayfinder #51)

> **Throwaway artifact.** A binding table + help-overlay mockups + the reveal interaction, to
> react to. Lives on branch `prototype/keymap`. The *decisions* fold into the implementation
> spec (#53); this file is captured as a primary source and does not land on `trunk`.
>
> **Question (#51):** What is the single, fixed, cross-screen keymap?

This is the authority for the chords #50 (layout) and #52 (autocomplete) referenced
illustratively, and for the "show hidden files" toggle #52 explicitly deferred here.

---

## 0. Hard constraints (from the foundation research note, #46)

Never bind these — terminal-reserved / ambiguous:

- `Ctrl-C` (SIGINT), `Ctrl-Z` (SIGTSTP), `Ctrl-S`/`Ctrl-Q` (XON/XOFF), `Ctrl-\` (SIGQUIT).
  **Exception:** users *expect* `Ctrl-C` to quit — so we honor it as the global quit/abort, but
  bind nothing else to it.
- `Ctrl-M` ≡ **Enter**, `Ctrl-I` ≡ **Tab**, `Ctrl-[` ≡ **Esc** on plain terminals. So
  **`Ctrl-Enter` is not a reliable distinct chord** (it collapses to Enter) — #50's mockups used
  `Ctrl-Enter start`; this prototype replaces it (see §2, REVIEW launch = the uniform forward chord).
- Baseline is **Press-only** (no key-release/repeat on non-kitty terminals). No binding may depend
  on release events or the kitty protocol; kitty niceties stay progressive enhancement.

Design axis, chosen to survive text-entry screens: **plain keys are consumed by text fields and
type-to-narrow, so every *global* action (step nav, quit, help, reveal) uses a Ctrl / Alt / Fn
chord** that stays live even while a passphrase or path field has focus.

---

## 1. The keymap (canonical, cross-screen)

Same action → same chord on every screen. Context columns note where a key's meaning specializes.

### Movement (in-screen)

| Key            | Action                                                      |
|----------------|------------------------------------------------------------|
| `↑` / `↓`      | Move cursor (scroll-on-margin, clamp at ends — ADR-0007)   |
| `PgUp`/`PgDn`  | Page the list                                              |
| `Home`/`End`   | Jump to first / last row                                   |
| `Tab`/`Shift-Tab` | Toggle focus between the two WHAT panes (Categories ↔ explorer). *In the custom-path field this is LCP-complete, per #52 — a text-entry sub-mode override.* |
| `→` / `Enter`  | Descend into the focused directory / open. *On a Category (left pane): move focus into its explorer.* *In the custom-path field `→` accepts ghost-text, per #52.* |
| `←` / `Backspace` | Climb one directory level. *At the explorer root: return focus to the Categories pane.* |
| *(letters)*    | Type-to-narrow: jump to the next row matching the typed prefix. *In a text field: insert.* |

### Selection

| Key       | Action                                                                        |
|-----------|-------------------------------------------------------------------------------|
| `Space`   | Toggle the current row. **On a directory this toggles the whole subtree** (the root-level bulk deselect, prefix-wins per ADR-0006). |
| `Ctrl-A`  | Select all rows in the focused list / directory.                              |
| `Ctrl-D`  | Deselect all rows in the focused list / directory. *(Shadows EOF, but our text fields don't use EOF — safe.)* |

### Wizard steps (global — work even inside a text field)

| Key         | Action                                                                       |
|-------------|------------------------------------------------------------------------------|
| `Ctrl-N`    | **Next** step (forward, completion-gated — ADR-0007 D2). **On REVIEW: launch the backup.** Disabled-with-reason when the current step is incomplete. |
| `Ctrl-P`    | **Previous** step (back — always free once a step is completed).             |
| `Alt-1..4`  | Jump directly to step 1–4 (WHAT / Encryption / WHERE / Review) if completed; no-op-with-hint if ahead of the frontier. Mirrors the jumpable header stepper. |

### Reveal / disclosure

| Key    | Action                                                                            |
|--------|----------------------------------------------------------------------------------|
| `F2`   | **Reveal**: toggle the passphrase field between masked (`••••`) and cleartext (§3). |
| `F3`   | **Show hidden files**: toggle dotfile visibility in the explorer / custom-path autocomplete (the #52-deferred "reveal-all" toggle). |

### Global

| Key      | Action                                                                          |
|----------|--------------------------------------------------------------------------------|
| `?`      | Toggle the help overlay (all bindings; §4).                                     |
| `Esc`    | Close an overlay/modal; else back out one context level (drilled → climb; at root → focus Categories). Never quits the app on its own. |
| `Ctrl-C` | Quit / abort. In the wizard: exit, nothing written. During **RUN**: cancel and remove the partial Backup dir (fail-fast, ADR-0002). |

### Modals (restore overwrite-confirm, plaintext-naming confirm)

| Key       | Action                                              |
|-----------|-----------------------------------------------------|
| `←` / `→` | Move between `[ Cancel ]` / `[ Confirm ]` buttons.  |
| `Enter`   | Activate the focused button.                        |
| `Esc`     | Cancel the modal.                                   |
| *(type)*  | Plaintext modal only: type `PLAINTEXT` to arm `[ Confirm ]`. |

### RUN screen

| Key      | Action                                                        |
|----------|--------------------------------------------------------------|
| `Ctrl-C` | Cancel (removes the partial Backup dir).                     |
| `Enter`  | On the done/summary state: close and exit.                  |

---

## 2. Why these choices (the non-obvious ones)

- **`Ctrl-N` / `Ctrl-P` for steps, not `Tab` / arrows.** `Tab` is pane-focus in the two-pane WHAT,
  and arrows are in-screen movement, so step nav needs its own axis. Ctrl-N/P is mnemonic
  (next/prev), unreserved, and — crucially — still fires while a WHERE path field or passphrase
  field has focus, keeping "advance the wizard" uniform on every screen.
- **REVIEW launch = `Ctrl-N`.** ADR-0007 D2: "completing REVIEW *is* launching the run." Reusing
  the forward chord means "advance" is one muscle-memory everywhere, and it dodges the unreliable
  `Ctrl-Enter`. A destructive-ish action guarded behind a dedicated REVIEW screen + an explicit
  forward chord (not a bare Enter) is deliberate.
- **`Esc` never quits.** It only backs out / closes. Quit is `Ctrl-C` alone, so an instinctive Esc
  never discards a half-built Selection.
- **Fn keys for reveal (`F2`/`F3`).** Both reveal actions happen on screens with active text entry
  (passphrase; autocomplete filtering), where plain letters are consumed — Fn keys stay free and
  are self-evidently "special", and F1–F4 are broadly supported.

---

## 3. Password reveal interaction (`F2`) & the SecretString caveat

The passphrase is held as `secrecy::SecretString` inside `Encryption` (unchanged). `F2` flips a
**view-only** `reveal: bool` in `App` — it never touches the secret itself.

```
masked (default):   Passphrase  [ ••••••••••••          ]  (F2 reveal)
revealed (F2):      Passphrase  [ correct-horse-battery ]  (F2 hide)
```

- On render **while revealed**, the field formats `secret.expose_secret()` for **that frame only**;
  the exposed `&str` is never cloned into a retained `String`, logged, or persisted.
- Default is **always masked**; reveal is a momentary, explicit user action, never the resting state.
- The value appears only in ratatui's **alternate screen** — it is not written to the terminal's
  normal scrollback, and the alt screen is discarded on teardown (ADR-0005 panic-safe restore).
- **Caveat, stated in-UI:** while revealed the passphrase is visible on screen (shoulder-surfing);
  that is the whole point of the toggle, so the only mitigation is the user's environment. A small
  hint sits by the field: `F2 hide` (so the revealed state advertises how to re-mask).
- Confirm-field match indicator (`✓ match` / `✗ …`) is computed on the `SecretString`s, independent
  of the reveal state.

---

## 4. Discoverability — footer + `?` overlay, across sizes (ties to #50)

Two layers: the **footer** always shows the handful of chords that matter *here*; **`?`** opens the
full reference. Footer content degrades on the #50 reflow ladder; `?` is always available.

**Footer, WHAT (Default height)** — the essential subset, context-aware:
```
│ Tab focus · ↑↓ move · Space toggle · → into dir · Ctrl-N next · ? keys       │
```

**Footer, Tight/Minimum** — trims to the irreducible core, always keeping `? keys`:
```
│ ↑↓ · Space · Ctrl-N next · ? keys                                            │
```

**`?` help overlay** — a centered panel grouping every binding; `Esc`/`?` closes. Scrolls if the
terminal is short; at Minimum size it fills the screen.
```
┌─ Keys ──────────────────────────────────────────────────────────────────────┐
│  Move        ↑↓ · PgUp/PgDn · Home/End · type to jump                        │
│  Panes       Tab / Shift-Tab focus · → into dir · ← up a level               │
│  Select      Space toggle (dir = whole subtree) · Ctrl-A all · Ctrl-D none   │
│  Steps       Ctrl-N next · Ctrl-P back · Alt-1..4 jump · (REVIEW: Ctrl-N run)│
│  Reveal      F2 passphrase · F3 hidden files                                 │
│  Global      ? keys · Esc back/close · Ctrl-C quit                           │
│                                                                             │
│  Reserved by the terminal (never bound): Ctrl-Z · Ctrl-S · Ctrl-Q · Ctrl-\  │
├─────────────────────────────────────────────────────────────────────────────┤
│ Esc or ? to close                                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 5. Consistency ledger (context overrides, justified)

The only keys whose meaning shifts by context — each an intuitive specialization, not a clash:

| Key     | Default meaning        | Override (where)                    | Why it reads the same |
|---------|------------------------|-------------------------------------|-----------------------|
| `Tab`   | Toggle pane focus      | LCP-complete (custom-path field, #52) | "advance within the current thing" |
| `→`     | Descend into directory | Accept ghost-text (custom-path, #52); move between modal buttons | "go forward / into"   |
| `Enter` | Descend / open         | Activate button (modals); close (RUN done) | "activate the focused thing" |
| `Esc`   | Back out one level     | Close overlay/modal                 | "cancel / step out"   |

Everything else is invariant across all screens.

---

## Open decisions for the human

1. **Step-nav chords** — `Ctrl-N` / `Ctrl-P` (next/prev) + `Alt-1..4` jump. Good, or prefer a
   different axis (e.g. `Alt-→`/`Alt-←`, or `PgDn`/`PgUp` for steps)?
2. **vim aliases** — add `j/k/h/l` and `g/G` as aliases for arrows/Home-End on the list screens
   (they'd be shadowed by type-to-narrow — so `j` jumps to a "j…" row *and* moves down?). Include,
   or keep type-to-narrow clean and arrows-only?
3. **Reveal keys** — `F2` passphrase / `F3` hidden files. Fine, or prefer `Ctrl-`/`Alt-` letters?
4. **Bulk deselect** — `Ctrl-D` for deselect-all (shadows EOF in text fields, but we don't use it).
   Accept, or pick another (e.g. `Alt-A`)?
5. **REVIEW launch** — reuse `Ctrl-N` (uniform forward), or a distinct explicit key (e.g. a typed
   confirmation) for the one irreversible action?
