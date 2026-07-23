//! The backup wizard: a completion-frontier stepper over a persistent [`WizardState`].
//!
//! Per ADR-0007 the wizard is a fixed four-step graph — WHAT → encryption → WHERE → REVIEW,
//! the encryption step **always present** — driven by a completion frontier: forward motion
//! is gated by the current step's completion (steps complete in order), backward/jump to any
//! already-reachable step is free, and steps ahead of the frontier are locked. [`Wizard`]
//! owns the pure `handle_key` reducer and the derived stepper state the header renders;
//! [`WizardState`] is the ADR-0005 D3 aggregate the whole wizard is a view over — the
//! `SelectionModel`, the [`Encryption`] choice, and the WHERE Output location.
//!
//! The one cross-step dependency is WHAT → encryption via sensitivity (ADR-0007 D5). It is
//! handled entirely by *derivation*, not by a mutation that could go stale: encryption
//! completeness is recomputed from the live Selection each time it's asked, so a WHAT edit
//! that newly exposes Sensitive content a plaintext decision didn't cover collapses the
//! frontier back to encryption automatically, and a set passphrase — being encrypted already
//! — is a safe harbor the collapse can never reach.
//!
//! Screen *bodies* (the two-pane WHAT explorer, the encryption field, the WHERE picker, the
//! REVIEW summary) land in builds #68–#72; this module renders placeholders for them and
//! wires the gates/frontier they will drive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linux_backup_core::archive::Encryption;
use linux_backup_core::crypto;
use linux_backup_core::model::SelectionModel;
use linux_backup_core::Privilege;
use secrecy::SecretString;

use crate::encryption_screen::{Commit, EncryptionScreen, ModalOutcome};
use crate::path_autocomplete::Getenv;
use crate::what_screen::WhatScreen;

/// The four fixed wizard steps, in graph order (ADR-0007 D1). Order is conveyed by position
/// (the stepper is not numbered, #50), so the discriminant order is the render order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    /// Choose the Selection (Categories, Custom paths, Drill-down deselections).
    What,
    /// Decide encryption — always present, required for a Sensitive Selection, offered
    /// otherwise (ADR-0007 D4).
    Encryption,
    /// Choose the Output location the Backup dir is written under.
    Where,
    /// The terminal step: review the run, then launch it (launch lands in build #71).
    Review,
}

impl WizardStep {
    /// The steps in graph order — the render order of the header stepper.
    pub const ALL: [Self; 4] = [Self::What, Self::Encryption, Self::Where, Self::Review];

    /// Zero-based position in the graph, used for frontier comparisons and `1`–`4` jumps.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::What => 0,
            Self::Encryption => 1,
            Self::Where => 2,
            Self::Review => 3,
        }
    }

    /// The next step in graph order, or `None` at the terminal REVIEW step.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        match self {
            Self::What => Some(Self::Encryption),
            Self::Encryption => Some(Self::Where),
            Self::Where => Some(Self::Review),
            Self::Review => None,
        }
    }

    /// The previous step in graph order, or `None` at the first WHAT step.
    #[must_use]
    pub fn prev(self) -> Option<Self> {
        match self {
            Self::What => None,
            Self::Encryption => Some(Self::What),
            Self::Where => Some(Self::Encryption),
            Self::Review => Some(Self::Where),
        }
    }

    /// The step a `1`–`4` digit selects, or `None` for any other character.
    #[must_use]
    pub fn from_digit(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::What),
            '2' => Some(Self::Encryption),
            '3' => Some(Self::Where),
            '4' => Some(Self::Review),
            _ => None,
        }
    }

    /// The step's title as it appears in the header stepper (#50).
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::What => "WHAT",
            Self::Encryption => "Encryption",
            Self::Where => "WHERE",
            Self::Review => "Review",
        }
    }
}

/// A step's visual state in the header stepper (#50): the glyph and whether it's jumpable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// A completed, reachable step behind the frontier (`✓`, jumpable).
    Completed,
    /// The step the wizard is currently on (`●`, its title marked by color/contrast).
    Current,
    /// A step ahead of the frontier, not yet reachable (`·`, dimmed).
    Locked,
    /// The encryption step after a WHAT edit newly exposed Sensitive content a plaintext
    /// decision didn't cover (`!`, jumpable) — ADR-0007 D5. Its stepper glyph (and every other
    /// glyph) is resolved through the render's active glyph set in [`crate::app`] / #73.
    Invalidated,
}

/// The reason a forward move was refused, rendered as a centered header banner dismissed on
/// the next input (no timer) — the gate-reason copy is verbatim from #61.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReason {
    /// WHAT is incomplete: the Selection is empty (mirrors `CoreError::EmptySelection`).
    NothingSelected,
    /// Encryption is incomplete: a Sensitive Selection has no passphrase and Auto-insist has
    /// not been disabled.
    EnterPassphrase,
    /// Encryption forward refused because the passphrase and confirm fields differ (#61).
    PassphrasesDoNotMatch,
    /// WHERE is incomplete: no Output location has been chosen.
    MissingBackupLocation,
    /// WHERE is incomplete: an Output location is chosen but its parent-writable probe failed
    /// (build #70's gate — the location doesn't resolve to a writable directory).
    LocationNotWritable,
}

impl GateReason {
    /// The verbatim gate-reason message (#61 copy sheet).
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NothingSelected => "Nothing selected",
            Self::EnterPassphrase => "Enter a passphrase",
            Self::PassphrasesDoNotMatch => "Passphrases do not match",
            Self::MissingBackupLocation => "Missing backup location",
            Self::LocationNotWritable => "Location is not writable",
        }
    }
}

/// The ADR-0005 D3 wizard aggregate: everything the four steps are a view over, persisted for
/// the whole wizard so a backward jump re-renders an earlier step against populated choices.
#[derive(Debug, Clone)]
pub struct WizardState {
    selection: SelectionModel,
    encryption: Encryption,
    /// The chosen Output location (the WHERE step's product); `None` until WHERE is decided.
    location: Option<PathBuf>,
    /// Whether the chosen `location`'s parent-writable probe passed (build #70). The probe runs
    /// at the `tui` boundary (`core::location::ensure_writable`) and its result is pushed in via
    /// [`Self::set_location`]; the seed is optimistic (a pre-filled location is assumed usable
    /// until the WHERE screen's probe says otherwise), so the shell opens on a populated wizard.
    location_writable: bool,
    /// The Sensitive Entry set the current *plaintext* decision was made against (ADR-0007
    /// D5's recorded context). `Some(set)` means a plaintext decision exists and covers
    /// `set` — the non-Sensitive default covers the empty set, a disabled Auto-insist covers
    /// the named set. `None` means no plaintext decision has been made yet (a Sensitive
    /// Selection still awaiting a passphrase or a disable-confirmation). Meaningless while
    /// `encryption` is a passphrase (the safe harbor), and never consulted there.
    plaintext_baseline: Option<BTreeSet<PathBuf>>,
}

impl WizardState {
    /// Builds the aggregate for a Selection and an optional pre-chosen Output location,
    /// starting plaintext. The plaintext decision is pre-recorded (covering the empty set)
    /// only when nothing is Sensitive — so a non-Sensitive Selection is encryption-complete
    /// on arrival, while a Sensitive one starts undecided and must set a passphrase or disable
    /// Auto-insist before the encryption step completes (ADR-0007 D4).
    #[must_use]
    pub fn new(selection: SelectionModel, location: Option<PathBuf>) -> Self {
        let baseline = if sensitive_set(&selection).is_empty() {
            Some(BTreeSet::new())
        } else {
            None
        };
        let location_writable = location.is_some();
        Self {
            selection,
            encryption: Encryption::None,
            location,
            location_writable,
            plaintext_baseline: baseline,
        }
    }

    /// The owned Selection, read-only.
    #[must_use]
    pub fn selection(&self) -> &SelectionModel {
        &self.selection
    }

    /// Whether the Selection contains anything Sensitive — drives the lock on the Encryption
    /// stepper label (#50) and Auto-insist.
    #[must_use]
    pub fn is_sensitive(&self) -> bool {
        crypto::evaluate(&self.selection).encryption_required
    }

    /// WHAT gate: the Selection is non-empty (mirrors `CoreError::EmptySelection`, ADR-0007
    /// D2).
    #[must_use]
    pub fn what_complete(&self) -> bool {
        !self.selection.is_empty()
    }

    /// Encryption gate (ADR-0007 D2/D4/D5): a decision exists. A set passphrase is always
    /// complete (safe harbor); a plaintext decision is complete only while the live Sensitive
    /// set stays within the set that decision was made against — so newly-added credentials
    /// can never sail through on a stale plaintext decision.
    #[must_use]
    pub fn encryption_complete(&self) -> bool {
        if self.encryption.is_encrypted() {
            return true;
        }
        match &self.plaintext_baseline {
            Some(baseline) => sensitive_set(&self.selection).is_subset(baseline),
            None => false,
        }
    }

    /// Whether the encryption step is *invalidated* (ADR-0007 D5): a prior plaintext decision
    /// no longer covers the live Sensitive set. Distinct from a Sensitive Selection that was
    /// simply never decided (no plaintext baseline yet) — only a decision that *was* complete
    /// and got overtaken shows the `!` glyph.
    #[must_use]
    pub fn encryption_invalidated(&self) -> bool {
        if self.encryption.is_encrypted() {
            return false;
        }
        match &self.plaintext_baseline {
            Some(baseline) => !sensitive_set(&self.selection).is_subset(baseline),
            None => false,
        }
    }

    /// WHERE gate (ADR-0007 D2): an Output location is chosen, resolves, and its parent is
    /// writable. The parent-writable probe is `core::location::ensure_writable`, run at the
    /// `tui` boundary by the WHERE screen (build #70) and pushed in via [`Self::set_location`];
    /// the advisory free-space warning is deliberately *not* part of this gate — WHERE space
    /// never blocks (#61).
    #[must_use]
    pub fn where_complete(&self) -> bool {
        self.location.is_some() && self.location_writable
    }

    /// Whether `step`'s forward gate is satisfied. REVIEW is terminal — it never "completes"
    /// (completing it is launching the run, build #71), so it never counts toward the
    /// frontier, which keeps the frontier capped at REVIEW's position once the first three
    /// steps are done.
    #[must_use]
    pub fn step_complete(&self, step: WizardStep) -> bool {
        match step {
            WizardStep::What => self.what_complete(),
            WizardStep::Encryption => self.encryption_complete(),
            WizardStep::Where => self.where_complete(),
            WizardStep::Review => false,
        }
    }

    /// The completion frontier: the index of the first incomplete step (equivalently, how
    /// many leading steps are complete). Steps before it are complete and jumpable; the step
    /// at it is the furthest reachable; steps after it are locked.
    #[must_use]
    pub fn frontier(&self) -> usize {
        WizardStep::ALL
            .iter()
            .position(|&step| !self.step_complete(step))
            .unwrap_or_else(|| WizardStep::Review.index())
    }

    /// Whether `step` is reachable — at or before the frontier (ADR-0007 D2: backward/jump to
    /// any reachable step is free; ahead-of-frontier steps are locked).
    #[must_use]
    pub fn is_reachable(&self, step: WizardStep) -> bool {
        step.index() <= self.frontier()
    }

    /// The gate reason a failed forward move from `step` reports (#61 copy). REVIEW has no
    /// forward gate (it's terminal), so it maps to `None`. WHERE distinguishes "no location
    /// chosen" from "chosen but not writable" (build #70) so the banner names the real fault.
    #[must_use]
    fn gate_reason(&self, step: WizardStep) -> Option<GateReason> {
        match step {
            WizardStep::What => Some(GateReason::NothingSelected),
            WizardStep::Encryption => Some(GateReason::EnterPassphrase),
            WizardStep::Where => {
                if self.location.is_some() && !self.location_writable {
                    Some(GateReason::LocationNotWritable)
                } else {
                    Some(GateReason::MissingBackupLocation)
                }
            }
            WizardStep::Review => None,
        }
    }
}

/// The mutation/accessor surface the per-step screen bodies drive (WHAT edits, the encryption
/// decision in build #72, the WHERE picker in build #70). The shell's own render reads only
/// the query methods above, so these are dead in this build's non-test code; they are wired
/// and unit-tested now because they *are* the ADR-0007 D5 safe-harbor/invalidation logic this
/// ticket owns, ready for the screens that call them.
#[allow(dead_code)]
impl WizardState {
    /// The owned Selection for in-place edits through its own public API (ADR-0005 D1: the
    /// model is only ever mutated via its own methods). Encryption completeness is re-derived
    /// on demand, so an edit here that changes sensitivity needs no companion bookkeeping —
    /// the frontier collapse of ADR-0007 D5 falls out of [`Self::encryption_complete`].
    pub fn selection_mut(&mut self) -> &mut SelectionModel {
        &mut self.selection
    }

    /// The current encryption choice.
    #[must_use]
    pub fn encryption(&self) -> &Encryption {
        &self.encryption
    }

    /// The chosen Output location, if WHERE has been decided.
    #[must_use]
    pub fn location(&self) -> Option<&Path> {
        self.location.as_deref()
    }

    /// Records a chosen Output location and its parent-writable probe result (the WHERE step's
    /// product, build #70). `writable` only counts when a location is actually present.
    pub fn set_location(&mut self, location: Option<PathBuf>, writable: bool) {
        self.location_writable = location.is_some() && writable;
        self.location = location;
    }

    /// Commits a passphrase — the encryption safe harbor (ADR-0007 D5). Once set, the whole
    /// Archive is encrypted, so no later WHAT edit can re-open the step or discard the typed
    /// passphrase.
    pub fn set_passphrase(&mut self, passphrase: SecretString) {
        self.encryption = Encryption::Passphrase(passphrase);
    }

    /// Records a deliberate plaintext decision covering the current Sensitive set — the
    /// non-Sensitive "keep plaintext" and the Sensitive disable-Auto-insist path both land
    /// here. A later WHAT edit that grows the Sensitive set beyond this baseline re-collapses
    /// the frontier (ADR-0007 D5).
    pub fn choose_plaintext(&mut self) {
        self.encryption = Encryption::None;
        self.plaintext_baseline = Some(sensitive_set(&self.selection));
    }
}

/// The Sensitive Entry set of a Selection, derived from `crypto::evaluate` (ADR-0007 D5: a
/// tui-side field derived from core's policy, core's `crypto` API unchanged).
fn sensitive_set(selection: &SelectionModel) -> BTreeSet<PathBuf> {
    crypto::evaluate(selection)
        .sensitive_entries
        .into_iter()
        .collect()
}

/// The wizard view: the persistent [`WizardState`], which step is current, and the transient
/// gate banner. This owns the pure `handle_key` reducer; the render lives in [`crate::app`].
///
/// Not `Clone`: [`WhatScreen`] holds a live worker channel once a probe is pending (ADR-0009),
/// which has no meaningful copy — see its own doc for the full rationale.
#[derive(Debug)]
pub struct Wizard {
    state: WizardState,
    current: WizardStep,
    gate_banner: Option<GateReason>,
    /// The WHAT step's own view state (focus, cursors, breadcrumb). Held for the whole wizard
    /// so a jump back to WHAT re-renders where the user left the explorer. Boxed to keep the
    /// `App`'s `View::Wizard` variant from dwarfing its sibling variants (`large_enum_variant`)
    /// as each build adds its step's view state here.
    what: Box<WhatScreen>,
    /// The encryption step's view state (input buffers, focus, the plaintext modal). Persists
    /// for the session so a backward jump to encryption re-renders the earlier field contents.
    /// Boxed to keep it off the [`crate::app::View`] enum's inline footprint.
    encryption_screen: Box<EncryptionScreen>,
}

impl Wizard {
    /// Opens the wizard at WHAT over `state`.
    #[must_use]
    pub fn new(state: WizardState) -> Self {
        let what = Box::new(WhatScreen::new(state.selection()));
        Self {
            state,
            current: WizardStep::What,
            gate_banner: None,
            what,
            encryption_screen: Box::new(EncryptionScreen::default()),
        }
    }

    /// The persistent aggregate.
    #[must_use]
    pub fn state(&self) -> &WizardState {
        &self.state
    }

    /// The WHAT step's view state, read by [`crate::app`]'s render for the two-pane explorer.
    #[must_use]
    pub fn what(&self) -> &WhatScreen {
        self.what.as_ref()
    }

    /// Forwards one poll tick into the WHAT step (ADR-0009 D2), regardless of which step is
    /// currently active — WHAT's pending probe outlives a step jump away from it, so
    /// `App::update`'s `Event::Tick` arm must keep draining it until it resolves even while the
    /// wizard is showing a different step.
    pub fn tick_what(&mut self) {
        self.what.tick();
    }

    /// The encryption step's view state, for its render (build #69).
    #[must_use]
    pub fn encryption_screen(&self) -> &EncryptionScreen {
        &self.encryption_screen
    }

    /// Records the WHERE step's chosen Output location and parent-writable probe result (build
    /// #70). `App` calls this after routing a key to the WHERE screen so the completion frontier
    /// tracks the live location; the advisory free-space warning is deliberately not threaded
    /// here (it never gates, #61).
    pub fn set_where(&mut self, location: Option<PathBuf>, writable: bool) {
        self.state.set_location(location, writable);
    }

    /// Folds one key into the WHAT step's Custom-path add field (#75). `App` routes here while
    /// the field is open (`self.what().is_typing()`), supplying the injected run facts and env
    /// resolver the pure `handle_key` reducer doesn't carry (ADR-0004). Adding a Custom path can
    /// newly expose Sensitive content, so the frontier is re-established afterward exactly as a
    /// WHAT edit through `handle_key` would (ADR-0007 D5).
    pub fn handle_what_field_key(
        &mut self,
        key: KeyEvent,
        home: &Path,
        privilege: Privilege,
        getenv: Getenv,
    ) {
        self.gate_banner = None;
        self.what
            .handle_field_key(key, &mut self.state, home, privilege, getenv);
        self.reestablish_frontier();
    }

    /// Flips the WHAT Custom-path field's reveal-hidden toggle (the `F3` reveal, #51). A no-op
    /// while the field is closed, so `App` can call it whenever `F3` fires on the WHAT step.
    pub fn toggle_what_show_hidden(&mut self) {
        self.what.toggle_custom_show_hidden();
    }

    /// Primes the WHAT Custom-path field with its privilege-aware `~/`/`/` prefill the instant
    /// it opens (#85). `App` calls this right after routing a plain key to `handle_key` (the
    /// only path that can newly open the field) with the injected [`Privilege`] the pure
    /// reducer doesn't carry (ADR-0004); it is a no-op on every other keystroke, since the
    /// field's buffer is only ever empty on the frame it opens.
    pub fn prime_what_custom_path(&mut self, privilege: Privilege) {
        self.what.prime_custom_path(privilege);
    }

    /// The step the wizard is currently on.
    #[must_use]
    pub fn current(&self) -> WizardStep {
        self.current
    }

    /// The gate banner to render, if a forward move was just refused.
    #[must_use]
    pub fn gate_banner(&self) -> Option<GateReason> {
        self.gate_banner
    }

    /// The stepper state to render for `step` (#50 glyph rules). Completed vs locked is
    /// decided by the *frontier*, not the step's own predicate: only steps strictly behind
    /// the frontier are complete-and-reachable (`✓`), while a step at or beyond it is either
    /// the current step (`●`), the invalidated encryption step (`!`, which wins over the
    /// current marker so a collapsed-to step reads as `!`), or locked-ahead (`·`). This is why
    /// a non-Sensitive encryption step whose predicate is trivially satisfied still reads as
    /// locked while WHAT is unfinished — you can't be *at* it yet.
    #[must_use]
    pub fn step_status(&self, step: WizardStep) -> StepStatus {
        if step == WizardStep::Encryption && self.state.encryption_invalidated() {
            StepStatus::Invalidated
        } else if step == self.current {
            StepStatus::Current
        } else if step.index() < self.state.frontier() {
            StepStatus::Completed
        } else {
            StepStatus::Locked
        }
    }

    /// The pure reducer: fold one key press into the next wizard state. Taking and returning
    /// `Self` by value keeps it a total function of its inputs, the property the reducer tests
    /// rely on. Any handled key first dismisses a standing gate banner (event-dismissed, no
    /// timer — #61); the action may then raise a fresh one.
    ///
    /// `Ctrl-C` is the runtime's sole quit and is intercepted upstream in [`crate::app`], so
    /// it never reaches here.
    #[must_use]
    pub fn handle_key(mut self, key: KeyEvent) -> Self {
        self.gate_banner = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // On the encryption step the screen body owns the plain keys (typing into the fields,
        // the modal), so route there; the global step chords stay live inside it (build #69).
        // The frontier is still re-established afterward so an encryption decision that changes
        // reachability can't leave `current` stranded ahead of it.
        if self.current == WizardStep::Encryption {
            self.handle_encryption_key(key, ctrl);
            self.reestablish_frontier();
            return self;
        }
        let mut consumed = true;
        match key.code {
            // Esc means back/close, never quit (ADR-0005 D-keymap): step back one, inert at
            // WHAT where there is nothing to close to.
            KeyCode::Esc => self.step_back(),
            // Ctrl-→ / Ctrl-← are the step-motion chords (they live on Ctrl so they stay off
            // the plain →/← the focused widget owns, #51).
            KeyCode::Right if ctrl => self.step_forward(),
            KeyCode::Left if ctrl => self.step_back(),
            // 1–4 jump directly to a reachable step. The #51 rule to suppress these while a
            // text field has focus is a no-op here — the WHAT screen has no focused field; the
            // encryption/WHERE screens (builds #70/#72) reinstate the suppression.
            KeyCode::Char(c) if !ctrl && WizardStep::from_digit(c).is_some() => {
                if let Some(target) = WizardStep::from_digit(c) {
                    self.jump_to(target);
                }
            }
            _ => consumed = false,
        }
        // Every key the global chords didn't claim belongs to the current step's body. Only
        // WHAT has a real body in this build; its reducer edits the Selection through the
        // model's public API (builds #70/#72 route their own steps here).
        if !consumed && self.current == WizardStep::What {
            self.what.handle_key(key, &mut self.state);
        }
        // A WHAT edit can lower the completion frontier (empty Selection, or newly-Sensitive
        // content invalidating a plaintext encryption decision, ADR-0007 D5). Re-establish the
        // frontier invariant so `current` never sits ahead of a step the edit just made
        // unreachable — the shell never needed this while step bodies were placeholders that
        // couldn't touch the Selection.
        self.reestablish_frontier();
        self
    }

    /// Pulls `current` back onto the frontier if a Selection edit collapsed it below where the
    /// wizard was sitting (ADR-0007 D5). Kept as a reducer postcondition rather than folded
    /// into the WHAT branch so any future step that edits the Selection is covered too.
    fn reestablish_frontier(&mut self) {
        let frontier = self.state.frontier();
        if self.current.index() > frontier {
            self.current = WizardStep::ALL[frontier];
        }
    }

    /// Attempts a gated forward move. Advances only if the current step is complete and has a
    /// successor; a refused move raises the step's gate banner instead (#61).
    fn step_forward(&mut self) {
        if self.state.step_complete(self.current) {
            if let Some(next) = self.current.next() {
                self.current = next;
            }
        } else {
            self.gate_banner = self.state.gate_reason(self.current);
        }
    }

    /// Moves back one step (always free — the previous step is reachable by construction).
    fn step_back(&mut self) {
        if let Some(prev) = self.current.prev() {
            self.current = prev;
        }
    }

    /// Jumps to `target` if it is reachable; a jump to a locked step is a no-op.
    fn jump_to(&mut self, target: WizardStep) {
        if self.state.is_reachable(target) {
            self.current = target;
        }
    }

    /// Folds one key while the encryption step is current (build #69). Global chords keep their
    /// meaning (`Ctrl-→` forward, `Ctrl-←`/`Esc` back); everything else goes to the screen body.
    /// A plain key the body doesn't consume falls back to the `1`–`4` step-jump, so a digit is
    /// suppressed exactly while a text field has focus (#51).
    fn handle_encryption_key(&mut self, key: KeyEvent, ctrl: bool) {
        // The plaintext modal is captive: every key routes to it while it is open.
        if self.encryption_screen.is_modal_open() {
            if self.encryption_screen.handle_modal_key(key) == ModalOutcome::ConfirmPlaintext {
                // Disabling Auto-insist for a Sensitive Selection: a plaintext decision covering
                // the named set (ADR-0007's second-confirmation escape).
                self.state.choose_plaintext();
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.step_back(),
            KeyCode::Right if ctrl => self.encryption_forward(),
            KeyCode::Left if ctrl => self.step_back(),
            // Swallow the other global chords (e.g. Ctrl-A/Ctrl-D) so they never type a char.
            _ if ctrl => {}
            _ => {
                let sensitive = self.state.is_sensitive();
                let consumed = self.encryption_screen.handle_plain_key(key, sensitive);
                if !consumed
                    && let KeyCode::Char(c) = key.code
                    && let Some(target) = WizardStep::from_digit(c)
                {
                    self.jump_to(target);
                }
            }
        }
    }

    /// The encryption step's gated forward move (build #69). Commits a typed passphrase (the
    /// safe harbor) or records the plaintext decision, then advances; a bad passphrase raises
    /// the specific gate banner (#61) instead.
    fn encryption_forward(&mut self) {
        let sensitive = self.state.is_sensitive();
        if sensitive {
            match self.encryption_screen.commit() {
                Commit::Passphrase(secret) => {
                    self.state.set_passphrase(secret);
                    self.advance_from_encryption();
                }
                // An empty field is fine only if a decision already exists — a passphrase set
                // earlier, or plaintext confirmed through the modal (both make it complete).
                Commit::Empty => {
                    if self.state.encryption_complete() {
                        self.advance_from_encryption();
                    } else {
                        self.gate_banner = Some(GateReason::EnterPassphrase);
                    }
                }
                Commit::Mismatch => self.gate_banner = Some(GateReason::PassphrasesDoNotMatch),
            }
        } else if self.encryption_screen.encrypt_opt_in() {
            match self.encryption_screen.commit() {
                Commit::Passphrase(secret) => {
                    self.state.set_passphrase(secret);
                    self.advance_from_encryption();
                }
                Commit::Empty => self.gate_banner = Some(GateReason::EnterPassphrase),
                Commit::Mismatch => self.gate_banner = Some(GateReason::PassphrasesDoNotMatch),
            }
        } else {
            // Offered mode, staying plaintext: record the decision (resets any prior passphrase)
            // and advance. Non-Sensitive, so the baseline covers the empty Sensitive set.
            self.state.choose_plaintext();
            self.advance_from_encryption();
        }
    }

    /// Advances off the encryption step to WHERE once a decision exists.
    fn advance_from_encryption(&mut self) {
        if let Some(next) = WizardStep::Encryption.next() {
            self.current = next;
        }
    }
}

#[cfg(test)]
impl Wizard {
    /// Test-only mutable access to the WHAT step's view state, so other modules' tests (e.g.
    /// `app::needs_poll`/`Event::Tick` coverage) can set up a pending-probe scenario via
    /// [`WhatScreen::set_pending_probe`] without a production request path to hang it off yet
    /// (#96).
    pub(crate) fn what_mut(&mut self) -> &mut WhatScreen {
        &mut self.what
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use linux_backup_core::model::{Entry, EntryRoot, EntryType};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        key(code, KeyModifiers::CONTROL)
    }

    fn entry(category: &'static str, sensitive: bool, name: &str) -> Entry {
        Entry {
            absolute_path: PathBuf::from(format!("/home/tester/{name}")),
            relative_path: PathBuf::from(name),
            root: EntryRoot::Home,
            entry_type: EntryType::File,
            category,
            sensitive,
        }
    }

    fn non_sensitive_selection() -> SelectionModel {
        SelectionModel::new(vec![entry("Shell", false, ".zshrc")])
    }

    fn sensitive_selection() -> SelectionModel {
        SelectionModel::new(vec![entry("SSH", true, "id_ed25519")])
    }

    /// A wizard on a non-Sensitive Selection with a location already chosen — so WHAT,
    /// encryption (plaintext complete on arrival), and WHERE are all complete and REVIEW is
    /// reachable.
    fn fully_reachable_wizard() -> Wizard {
        let state = WizardState::new(
            non_sensitive_selection(),
            Some(PathBuf::from("/home/tester")),
        );
        Wizard::new(state)
    }

    #[test]
    fn non_sensitive_selection_is_encryption_complete_on_arrival() {
        let state = WizardState::new(non_sensitive_selection(), None);
        assert!(state.encryption_complete());
        assert!(!state.encryption_invalidated());
    }

    #[test]
    fn sensitive_selection_starts_encryption_incomplete() {
        let state = WizardState::new(sensitive_selection(), None);
        assert!(!state.encryption_complete());
        // Never decided is not the same as invalidated — no `!` glyph, just current.
        assert!(!state.encryption_invalidated());
    }

    #[test]
    fn empty_selection_makes_what_incomplete_and_frontier_zero() {
        let state = WizardState::new(SelectionModel::new(vec![]), None);
        assert!(!state.what_complete());
        assert_eq!(state.frontier(), WizardStep::What.index());
    }

    #[test]
    fn forward_is_gated_by_completion_and_reports_the_reason() {
        // Empty Selection: WHAT incomplete, forward refused with `Nothing selected`.
        let wizard = Wizard::new(WizardState::new(SelectionModel::new(vec![]), None));
        let wizard = wizard.handle_key(ctrl(KeyCode::Right));
        assert_eq!(wizard.current(), WizardStep::What);
        assert_eq!(wizard.gate_banner(), Some(GateReason::NothingSelected));
    }

    #[test]
    fn forward_advances_once_the_step_completes() {
        let wizard = fully_reachable_wizard();
        let wizard = wizard.handle_key(ctrl(KeyCode::Right));
        assert_eq!(wizard.current(), WizardStep::Encryption);
        assert_eq!(wizard.gate_banner(), None);
    }

    #[test]
    fn any_key_dismisses_a_standing_gate_banner() {
        let wizard = Wizard::new(WizardState::new(SelectionModel::new(vec![]), None));
        let wizard = wizard.handle_key(ctrl(KeyCode::Right));
        assert!(wizard.gate_banner().is_some());
        // The next input dismisses it (event-dismissed, no timer).
        let wizard = wizard.handle_key(ctrl(KeyCode::Left));
        assert_eq!(wizard.gate_banner(), None);
    }

    #[test]
    fn backward_jump_to_a_completed_step_is_free() {
        let wizard = fully_reachable_wizard()
            .handle_key(ctrl(KeyCode::Right)) // → Encryption
            .handle_key(ctrl(KeyCode::Right)); // → WHERE
        assert_eq!(wizard.current(), WizardStep::Where);
        let wizard = wizard.handle_key(ctrl(KeyCode::Left));
        assert_eq!(wizard.current(), WizardStep::Encryption);
    }

    #[test]
    fn digit_jumps_to_a_reachable_step_and_ignores_locked_steps() {
        let wizard = fully_reachable_wizard();
        // '4' → REVIEW is reachable (WHAT/enc/WHERE all complete), jump lands.
        let wizard = wizard.handle_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
        assert_eq!(wizard.current(), WizardStep::Review);

        // On an empty Selection only WHAT is reachable; '3' → WHERE is locked, no-op.
        let locked = Wizard::new(WizardState::new(SelectionModel::new(vec![]), None));
        let locked = locked.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(locked.current(), WizardStep::What);
    }

    #[test]
    fn esc_steps_back_and_never_quits_at_the_first_step() {
        let wizard = fully_reachable_wizard().handle_key(ctrl(KeyCode::Right)); // → Encryption
        let wizard = wizard.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(wizard.current(), WizardStep::What);
        // Esc at WHAT is inert (nothing to close to); it certainly does not quit.
        let wizard = wizard.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(wizard.current(), WizardStep::What);
    }

    #[test]
    fn adding_sensitive_content_to_a_plaintext_decision_invalidates_encryption() {
        // Non-Sensitive → encryption complete (plaintext, covers the empty set).
        let mut state = WizardState::new(non_sensitive_selection(), None);
        assert!(state.encryption_complete());

        // A WHAT edit introduces Sensitive content the plaintext decision didn't cover.
        state.selection_mut().add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );
        // The frontier collapses back to encryption, and the step reads as invalidated.
        // (add_custom_path derives sensitivity via the Safety-net scan, so id_ed25519 is
        // Sensitive.)
        assert!(!state.encryption_complete());
        assert!(state.encryption_invalidated());
        assert_eq!(state.frontier(), WizardStep::Encryption.index());
    }

    #[test]
    fn a_set_passphrase_is_a_safe_harbor_against_invalidation() {
        let mut state = WizardState::new(non_sensitive_selection(), None);
        state.set_passphrase(SecretString::from("correct-horse-battery-staple9!"));
        assert!(state.encryption_complete());

        // Adding Sensitive content never re-opens an already-encrypting decision.
        state.selection_mut().add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );
        assert!(state.encryption_complete());
        assert!(!state.encryption_invalidated());
    }

    #[test]
    fn disabling_auto_insist_covers_the_named_set_but_not_a_later_addition() {
        let mut state = WizardState::new(sensitive_selection(), None);
        assert!(!state.encryption_complete());

        // Disable Auto-insist: a plaintext decision covering the current Sensitive set.
        state.choose_plaintext();
        assert!(state.encryption_complete());
        assert!(!state.encryption_invalidated());

        // A further Sensitive addition grows the set beyond the baseline → invalidated again.
        // `.aws/credentials` trips the Safety-net scan on the `credential` keyword.
        state.selection_mut().add_custom_path(
            Path::new("/home/tester/.aws/credentials"),
            Path::new("/home/tester"),
        );
        assert!(state.encryption_invalidated());
    }

    #[test]
    fn a_selection_edit_pulls_current_back_onto_the_collapsed_frontier() {
        // Sit at REVIEW with everything complete (frontier at REVIEW).
        let wizard =
            fully_reachable_wizard().handle_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
        assert_eq!(wizard.current, WizardStep::Review);

        // A WHAT edit empties the Selection, collapsing the frontier back to WHAT. The next
        // key must re-establish the invariant and pull `current` back off the now-locked step.
        let mut wizard = wizard;
        wizard.state = WizardState::new(SelectionModel::new(vec![]), None);
        let wizard = wizard.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(wizard.current, WizardStep::What);
        assert!(wizard.current.index() <= wizard.state.frontier());
    }

    #[test]
    fn step_status_marks_completed_current_locked_and_invalidated() {
        // Empty Selection: WHAT current+incomplete, everything after it locked.
        let wizard = Wizard::new(WizardState::new(SelectionModel::new(vec![]), None));
        assert_eq!(wizard.step_status(WizardStep::What), StepStatus::Current);
        assert_eq!(
            wizard.step_status(WizardStep::Encryption),
            StepStatus::Locked
        );

        // Fully reachable, sitting on WHERE: WHAT/encryption completed behind the frontier.
        let wizard = fully_reachable_wizard()
            .handle_key(ctrl(KeyCode::Right))
            .handle_key(ctrl(KeyCode::Right));
        assert_eq!(wizard.current(), WizardStep::Where);
        assert_eq!(wizard.step_status(WizardStep::What), StepStatus::Completed);
        assert_eq!(
            wizard.step_status(WizardStep::Encryption),
            StepStatus::Completed
        );
        assert_eq!(wizard.step_status(WizardStep::Where), StepStatus::Current);

        // Invalidated encryption wins over the current marker.
        let mut state = WizardState::new(non_sensitive_selection(), None);
        state.selection_mut().add_custom_path(
            Path::new("/home/tester/.ssh/id_ed25519"),
            Path::new("/home/tester"),
        );
        let invalidated =
            Wizard::new(state).handle_key(key(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(invalidated.current(), WizardStep::Encryption);
        assert_eq!(
            invalidated.step_status(WizardStep::Encryption),
            StepStatus::Invalidated
        );
    }
}
