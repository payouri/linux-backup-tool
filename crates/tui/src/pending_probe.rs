//! [`PendingProbe`]: the generic worker-thread + `mpsc` primitive WHAT's own slow, disk-touching
//! probes ride (ADR-0009 D1). Unlike [`crate::run_screen::BackupWorker`] — one hardcoded job
//! type, because RUN only ever has one job in flight — WHAT has several candidate probe kinds
//! (a per-file size `stat`, a Selection-wide total, a possible future taxonomy rescan), so this
//! primitive is op-agnostic: it spawns any `FnOnce() -> T + Send` on a `std::thread` and hands
//! `T` back over an `mpsc::channel`, recording its own spawn `Instant` for the caller's debounce
//! math (ADR-0009 D3). This module ships the primitive only — which ops ride it (#96), the
//! debounce/spinner render (#94), are downstream decisions.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One in-flight probe: a `T` being computed off-thread, delivered back over a channel. Its
/// owner holds it as a single `Option<PendingProbe<T>>` (ADR-0009 D2) — replacing the option
/// with a fresh probe drops this one, which drops its `Receiver`; the orphaned thread finishes
/// its blocking call, attempts a `send` nobody receives, and exits (ADR-0009 D4). There is no
/// cancellation to wire: WHAT's probes are read-only `stat`-class calls, cheap enough that
/// letting an abandoned one run to completion and quietly discard its result is simpler and
/// just as correct as interrupting it.
#[derive(Debug)]
pub struct PendingProbe<T> {
    rx: Receiver<T>,
    // Kept alive so the thread isn't detached mid-probe; never joined explicitly — a `send`
    // into a dropped `Receiver` (the abandonment path) is not observed as a joined result.
    _handle: JoinHandle<()>,
    started: Instant,
}

impl<T: Send + 'static> PendingProbe<T> {
    /// Spawns `op` on a `std::thread` and returns a handle for polling its result via
    /// [`PendingProbe::try_recv`]. `op` always runs to completion, whether or not anything ever
    /// reads the result.
    // No production call site exists yet: WHAT has no op wired through this primitive until #96
    // decides which ones do (this ticket lands the spine only). Exercised by this module's own
    // tests below.
    #[allow(dead_code)]
    pub fn spawn<F>(op: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            // A send failure means the receiver was dropped (replaced or abandoned) — the
            // computed value has nowhere to go and is simply discarded.
            let _ = tx.send(op());
        });
        Self {
            rx,
            _handle: handle,
            started: Instant::now(),
        }
    }

    /// Non-blocking poll of the result — the caller drains this each tick.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.rx.try_recv()
    }

    /// How long this probe has been in flight, for the caller's debounce threshold (ADR-0009
    /// D3: a spinner becomes visible once this crosses 375ms) — read by
    /// [`crate::what_screen::WhatScreen`]'s own debounce check (#94).
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks until `probe` resolves or `timeout` elapses, polling `try_recv` — avoids a fixed
    /// `sleep` racing the worker thread's own scheduling.
    fn drain_within<T: Send + 'static>(probe: &PendingProbe<T>, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            match probe.try_recv() {
                Ok(value) => return Some(value),
                Err(TryRecvError::Empty) if Instant::now() < deadline => thread::yield_now(),
                Err(_) => return None,
            }
        }
    }

    #[test]
    fn spawn_runs_any_send_fn_once_and_delivers_its_result() {
        // Generic over any `FnOnce() -> T + Send`: a plain closure here, a `WhatProbeResult` in
        // WHAT's real use.
        let probe = PendingProbe::spawn(|| 2 + 2);
        assert_eq!(
            drain_within(&probe, Duration::from_secs(1)),
            Some(4),
            "expected the spawned closure's result over the channel"
        );
    }

    #[test]
    fn try_recv_is_empty_before_the_thread_finishes() {
        // A probe that blocks until told to proceed: `try_recv` must not block waiting for it.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let probe = PendingProbe::spawn(move || {
            let _ = release_rx.recv();
            1
        });
        assert!(matches!(probe.try_recv(), Err(TryRecvError::Empty)));
        let _ = release_tx.send(());
        assert_eq!(drain_within(&probe, Duration::from_secs(1)), Some(1));
    }

    #[test]
    fn elapsed_grows_from_the_spawn_instant() {
        let probe = PendingProbe::spawn(|| ());
        thread::sleep(Duration::from_millis(5));
        assert!(probe.elapsed() >= Duration::from_millis(5));
    }
}
