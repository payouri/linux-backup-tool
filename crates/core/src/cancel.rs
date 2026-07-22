//! `CancelToken`: the cooperative-cancellation signal a caller flips to ask an in-flight
//! `backup`/`restore` run to stop at its next checkpoint.
//!
//! Passed as its own parameter to [`backup`](crate::backup)/[`restore`](crate::restore::restore),
//! deliberately *not* on [`RunContext`](crate::context::RunContext): per ADR-0004 that struct
//! carries ambient run *facts* fixed for the whole run, whereas this is live control input a
//! second thread mutates mid-run. Keeping it separate also keeps [`crate::progress::Progress`]
//! strictly one-way (observation) — observation and control travel on different channels.
//!
//! `core` only ever *reads* the flag (at its documented checkpoints); the caller owning the run
//! (the TUI/GUI worker) is the only side that flips it. It is std-atomics only — no new
//! dependency — so `#![forbid(unsafe_code)]` stays intact.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared, thread-safe "please stop" flag for one backup or restore run.
///
/// Cloning shares the underlying flag (it's an `Arc`), so the thread running `core` and the
/// thread that decides to cancel hold two handles to the *same* signal. A freshly-constructed
/// token is not cancelled; callers that never cancel (the TUI today, most tests) construct one
/// and simply never call [`cancel`](Self::cancel).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A new, not-yet-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Idempotent; safe to call from any thread. `core` observes it at
    /// its next checkpoint — this never interrupts work already in progress.
    pub fn cancel(&self) {
        // Relaxed suffices: the flag gates control flow only and publishes no other data, so no
        // happens-before relationship with surrounding memory needs establishing here.
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested — the check `core` polls at its checkpoints.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_token_is_not_cancelled() {
        assert!(!CancelToken::new().is_cancelled());
    }

    #[test]
    fn cancel_is_visible_through_a_clone() {
        let token = CancelToken::new();
        let handle = token.clone();
        token.cancel();
        assert!(
            handle.is_cancelled(),
            "a clone must observe cancellation on the shared flag"
        );
    }
}
