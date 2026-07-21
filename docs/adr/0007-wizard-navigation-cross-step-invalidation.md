# The wizard is a completion-frontier stepper with targeted cross-step invalidation

**Status:** Accepted · **Date:** 2026-07-21

The ratatui wizard ([ADR-0005](0005-ratatui-app-architecture-core-tui-boundary.md)) is
back-navigable over a persistent `WizardState`, but ADR-0005 D3 deliberately left the *transition*
and *cross-step invalidation* rules undecided and handed them to
[#49](https://github.com/payouri/linux-backup-tool/issues/49). This ADR fixes those rules. It
governs `tui` only (the wizard is a view over core's `SelectionModel` and `crypto` policy) and is
one of the decisions the implementation spec ([#53](https://github.com/payouri/linux-backup-tool/issues/53))
rests on.

## Decisions

### 1. The step graph is fixed at four always-present steps

WHAT → encryption → WHERE → REVIEW. The encryption step is **always present**, not conditional on
sensitivity — this removes any materializing/vanishing step from the graph and keeps the transition
rules independent of the Selection's contents. What *changes* with sensitivity is the encryption
step's behavior, not its existence (Decision 4).

### 2. Movement is a completion frontier

- **Forward is gated by completion.** You advance to the next step only once the current step is
  complete; steps complete in order. An incomplete step's forward move reads as disabled-with-reason.
- **Backward/jump to any already-completed step is free.** Once REVIEW is reached, WHAT, encryption,
  and WHERE are all directly jumpable. Ahead-of-frontier steps stay locked.

Per-step completeness — the forward gate:

- **WHAT** ⇔ the Selection is non-empty. This mirrors core's own `CoreError::EmptySelection` guard
  (`lib.rs`), so the wizard never lets you reach a run core would reject.
- **encryption** ⇔ a decision exists. When the Selection is Sensitive: a passphrase is set **or**
  Auto-insist was disabled through the second plaintext-naming confirmation (unchanged from the
  `inquire` flow). When non-Sensitive: **complete on arrival** (default `Encryption::None`) — see
  Decision 4.
- **WHERE** ⇔ a location is chosen and passes cheap validation (path resolves, parent writable). The
  Backup-dir **collision** check ("refuses if the dir already exists") stays a run-launch check where
  it lives today — it is inherently racy and already core's job, not a wizard gate.
- **REVIEW** — terminal. "Completing" REVIEW *is* launching the run (transition to `Screen::Run`,
  ADR-0005 D4).

### 3. State persists in `WizardState` for the wizard's lifetime

`WizardState` (ADR-0005 D3) holds the `SelectionModel`, the `Encryption` choice, and the WHERE
location for the whole wizard, so a backward jump re-renders an earlier step against already-populated
choices. The committed passphrase persists in-session (held as the `SecretString` inside `Encryption`),
so leaving and re-entering the encryption step shows it as already-set rather than blank. *How* a
set passphrase is re-presented (masked chip vs re-editable buffer) and the reveal toggle belong to
the layout/keymap tickets ([#50](https://github.com/payouri/linux-backup-tool/issues/50),
[#51](https://github.com/payouri/linux-backup-tool/issues/51)), not here.

### 4. Encryption is always offered; Auto-insist behavior is unchanged for Sensitive Selections

- **Sensitive Selection** → today's Auto-insist behavior, unchanged: the summary names the triggering
  Categories, and the user must set a passphrase or pass the second plaintext-naming confirmation to
  disable (`crypto::evaluate` / `AutoInsist`).
- **Non-Sensitive Selection** → encryption is *offered*, not forced: the user may set a passphrase to
  encrypt an otherwise-non-sensitive backup, or accept the default `Encryption::None`.

This is a small behavior addition over the `inquire` flow, where the encryption screen no-ops to
`Encryption::None` whenever nothing is Sensitive. *Downstream (implementation, tracked in the spec):*
the encryption screen no longer no-ops when `encryption_required == false` — it renders an optional
opt-in passphrase path. This is `tui`-side only; core's `Encryption` already carries the passphrase.

### 5. Cross-step invalidation is targeted, and a set passphrase is a safe harbor

A downstream completed step is invalidated by an edit to an earlier step **only when the edit changes
an input that step actually depends on**. The wizard has exactly one such cross-step dependency:
**WHAT → encryption, via the Selection's sensitivity.** WHERE depends on nothing upstream, so no WHAT
edit ever invalidates it.

The encryption step is invalidated **only when both** hold:

1. its current decision is **plaintext** (`Encryption::None` — whether the non-Sensitive default or a
   deliberately-disabled Auto-insist), **and**
2. a WHAT edit introduces Sensitive content that decision did not cover — the Selection went from
   nothing-Sensitive to Sensitive, or the Sensitive set grew beyond the Categories a prior
   disable-Auto-insist confirmation named.

On invalidation the encryption step drops to incomplete, the completion frontier collapses back to it,
and Auto-insist re-asserts — the user must re-decide encryption before REVIEW is reachable again.

**A set passphrase is a safe harbor:** if encryption is already ON, the whole archive is encrypted, so
newly-added Sensitive content is simply encrypted too and no WHAT edit re-opens the step. Because the
only states invalidation ever touches are already plaintext, the invalidation path never discards a
typed passphrase.

This is the rule ADR-0005 D3 flagged as open ("whether changing WHAT invalidates a downstream
encryption auto-insist"): it does, but narrowly — only when a plaintext decision would otherwise ship
newly-added credentials unencrypted.

### 6. Responsiveness bar

The bar the layout ([#50](https://github.com/payouri/linux-backup-tool/issues/50)) designs against,
and a property a `TestBackend` render assertion can check:

- **Redraw-on-event, not fixed-FPS**, on all interactive screens: the event loop blocks on input and
  issues exactly **one `terminal.draw()` per state change** — no idle repaint, no polling spin. The
  RUN screen is the sole exception (it polls the progress channel each tick, ADR-0005 D4). One draw
  per change is flicker-free by construction — ratatui double-buffers and diffs, so there is never a
  clear-then-repaint flash.
- **Key latency imperceptible (< ~50 ms)** — navigation keys are handled synchronously with no
  debounce. Debounce is autocomplete-only ([#52](https://github.com/payouri/linux-backup-tool/issues/52)).
- **List viewport: scroll-on-margin, clamp at ends (no wrap)** — the cursor stays within the list; the
  viewport scrolls only when the cursor reaches the top/bottom margin; moving past the last item
  clamps rather than wrapping. Uses ratatui's `ListState` scroll offset.
- **No reflow jump on step change** — the header (with the step indicator) and footer stay fixed;
  only the body region swaps between steps.

### 7. Affordance

A persistent horizontal stepper in the fixed header shows the four steps in three visual states:
**completed** (jumpable), **current**, and **locked/ahead-of-frontier** (dimmed, not selectable). The
footer shows the available moves for the current step, and the forward gate reads as
disabled-with-reason when the current step is incomplete. Concrete glyphs and colors belong to the
layout ticket (#50); chord assignments to the keymap ticket (#51).

## Consequences

- The implementation spec (#53) is authored against these rules and carries one named `tui`-side
  follow-up: the encryption screen must offer an opt-in passphrase path when encryption is not required
  (Decision 4).
- `WizardState` needs enough recorded context to evaluate Decision 5 — the Sensitive set the current
  encryption decision was made against — so a later WHAT edit can tell whether the Sensitive set *grew
  beyond* it. This is a `tui`-side field, derived from `crypto::evaluate`; core's `crypto` API is
  unchanged.
- WHERE is fully independent of WHAT, so the two can be edited in any order without cross-effects —
  the only ordering constraint the frontier enforces is that each step be *completed* before the next
  is first reached.
- This ADR does not amend ADR-0005; it fills the transition/invalidation hole ADR-0005 D3 left open.
