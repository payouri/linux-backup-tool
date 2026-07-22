//! The shared terminal runtime: written once, seeded by an initial [`Screen`].
//!
//! Per ADR-0005 / spec #62, the terminal lifecycle — raw mode, the alternate screen, a panic
//! hook that restores the terminal, the crossterm read loop, resize handling, and teardown —
//! lives in exactly one place. Every entry point (`backup`, `restore`, the dev `select`/
//! `where` screens) builds an [`App`] seeded with its initial [`Screen`] and hands it to
//! [`run`]; nothing else touches the terminal directly.
//!
//! This is a system-boundary module: per spec #62's Testing Decisions the real `read()` loop
//! and raw-mode/alt-screen lifecycle are intentionally kept thin and left off the automated
//! seam (exercised by hand against a real terminal), with the redraw-on-event logic and
//! rendering covered by the pure reducer and `TestBackend` tests in [`crate::app`].

use std::io::{self, Stdout};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event as CrosstermEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Event};
use crate::run_screen::{self, BackupWorker};

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// How long a RUN poll tick waits on crossterm before falling through to drain the progress
/// channel and repaint. Short enough that a keypress (Ctrl-C cancel) still feels immediate and
/// the spinner animates smoothly; long enough not to busy-spin the CPU during a long phase.
const RUN_POLL_INTERVAL: Duration = Duration::from_millis(80);

/// Drives an [`App`] to completion: sets up the terminal, runs the redraw-on-event loop until
/// the app requests quit (or the read loop errors), then restores the terminal regardless of
/// how the loop ended. Teardown runs even on a loop error so a failing run never leaves the
/// user's terminal in raw mode.
pub fn run(app: App) -> Result<()> {
    // Restore on a setup failure too: `enable_raw_mode` can succeed and a later step
    // (alt screen, backend) still fail, which the panic hook won't catch (it's an `Err`,
    // not a panic) — without this the shell is stranded in raw mode.
    let mut terminal = match init_terminal() {
        Ok(terminal) => terminal,
        Err(setup_error) => {
            let _ = restore_terminal();
            return Err(setup_error);
        }
    };
    let loop_result = run_loop(&mut terminal, app);
    // Restore unconditionally: a `?` on the loop result would skip teardown and strand the
    // terminal in raw mode / the alternate screen.
    let restore_result = restore_terminal();
    loop_result.and(restore_result)
}

/// The main loop. Every interactive screen is redraw-on-event: draw once, block on the next
/// input, fold it into state, repeat. The one exception is RUN, which polls: while a backup is
/// in flight the loop drains the worker's progress channel and crossterm together each tick
/// (spec #62 / ADR-0005 — RUN is the *only* polling screen), so the frame stays live during
/// the backup without any other screen spinning the CPU.
///
/// Only `KeyEventKind::Press` is forwarded — terminals that also report `Repeat`/`Release`
/// would otherwise double-handle a single keystroke. Non-key, non-resize events are ignored
/// (mouse input is out of scope, spec #62).
fn run_loop(terminal: &mut Tui, mut app: App) -> Result<()> {
    // Alive only while a backup runs; spawned when REVIEW hands over a launch request and
    // dropped once the run is terminal (the thread has already exited by then).
    let mut worker: Option<BackupWorker> = None;
    loop {
        terminal
            .draw(|frame| app.render(frame))
            .context("draw frame")?;

        if app.should_quit() {
            break;
        }

        // A REVIEW launch produces a one-shot request the first turn after the transition;
        // spawn the worker thread that drives `core::backup` over the progress channel.
        if let Some(request) = app.take_backup_request() {
            worker = Some(run_screen::spawn(request));
        }

        let next_event = if app.is_backup_running() {
            run_poll_tick(&mut app, worker.as_ref())?
        } else {
            // Back to (or still) redraw-on-event; a finished worker can be released here — its
            // thread already sent the terminal message and exited.
            worker = None;
            read_event()?
        };

        if let Some(event) = next_event {
            app = app.update(event);
        }
    }
    Ok(())
}

/// One RUN poll tick: drain every pending progress message into the app, animate the spinner,
/// then wait (briefly) for one input event, returning it for the caller to fold in by value. A
/// disconnected channel with no terminal message means the worker panicked, which reads as a
/// failed run.
fn run_poll_tick(app: &mut App, worker: Option<&BackupWorker>) -> Result<Option<Event>> {
    if let Some(worker) = worker {
        loop {
            match worker.try_recv() {
                Ok(message) => app.apply_run_message(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    app.on_worker_disconnected();
                    break;
                }
            }
        }
    }
    app.tick();

    if event::poll(RUN_POLL_INTERVAL).context("poll terminal event")? {
        read_event()
    } else {
        Ok(None)
    }
}

/// Reads one crossterm event and maps it to the app [`Event`] the reducer consumes, or `None`
/// for an event the app ignores (non-`Press` keys, mouse). Blocks until an event arrives, so
/// it is only called when the caller intends to wait (redraw-on-event, or after `poll`).
fn read_event() -> Result<Option<Event>> {
    Ok(match event::read().context("read terminal event")? {
        CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => Some(Event::Key(key)),
        CrosstermEvent::Resize(width, height) => Some(Event::Resize(width, height)),
        _ => None,
    })
}

fn init_terminal() -> Result<Tui> {
    // Installed before entering raw mode so a panic during setup or the loop still restores
    // the terminal instead of leaving the user staring at a raw-mode shell.
    install_panic_hook();
    enable_raw_mode().context("enable raw mode")?;
    execute!(io::stdout(), EnterAlternateScreen).context("enter alternate screen")?;
    Terminal::new(CrosstermBackend::new(io::stdout())).context("create terminal backend")
}

/// Leaves the alternate screen and disables raw mode. Also called from the panic hook, so it
/// must stay independent of any `App`/`Terminal` state.
fn restore_terminal() -> Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen).context("leave alternate screen")?;
    disable_raw_mode().context("disable raw mode")?;
    Ok(())
}

/// Chains terminal restoration in front of the previously-installed panic hook so a panic
/// anywhere in the app unwinds to a usable terminal before the default (or test) hook prints
/// the message. Restoration is best-effort — a failure to restore must not mask the panic.
fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));
}
