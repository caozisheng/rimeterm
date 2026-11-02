//! Safe-timeout driver for synchronized updates (mode 2026).
//!
//! vte's default `StdSyncHandler` records a deadline but never consults it,
//! so a producer that opens a BSU and stalls (compile-loaded agent) freezes
//! the grid until more bytes arrive. This module owns one `Processor` per
//! session plus one timer task: after each batch the reader publishes the
//! recorded deadline on a `watch` (`send_if_modified`, so unchanged
//! deadlines never wake the timer). At expiry the re-check and `stop_sync`
//! run on the blocking pool, never on a Tokio worker.
//!
//! Lock order is fixed — parser, then grid — and both are released before
//! the timeout callback runs. The timer ends when the driver (the watch
//! sender) is dropped.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Processor;
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::{Instant as TokioInstant, sleep_until};

use crate::session::{Listener, parse_chunks};

/// One parser plus the timer that breaks a stalled synchronized update.
pub(crate) struct SyncDriver {
    processor: Arc<Mutex<Processor>>,
    term: Arc<Mutex<Term<Listener>>>,
    deadlines: watch::Sender<Option<Instant>>,
}

impl SyncDriver {
    /// Spawn the timer on the current Tokio runtime; panics outside one.
    pub(crate) fn new<F>(term: Arc<Mutex<Term<Listener>>>, on_timeout: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        Self::new_with(&Handle::current(), term, on_timeout)
    }

    /// Runtime-explicit constructor; `Handle::spawn` works off-context.
    pub(crate) fn new_with<F>(
        handle: &Handle,
        term: Arc<Mutex<Term<Listener>>>,
        on_timeout: F,
    ) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let (deadlines, receiver) = watch::channel(None);
        let processor = Arc::new(Mutex::new(Processor::new()));
        handle.spawn(
            SyncTimer {
                deadlines: receiver,
                processor: Arc::clone(&processor),
                term: Arc::clone(&term),
                on_timeout: Arc::new(on_timeout),
            }
            .run(),
        );
        Self {
            processor,
            term,
            deadlines,
        }
    }

    /// Feed one PTY batch. Parser state survives batches and UTF-8 splits.
    pub(crate) fn advance(&self, bytes: &[u8]) {
        let mut parser = self.processor.lock();
        for chunk in parse_chunks(bytes) {
            let mut term = self.term.lock();
            parser.advance(&mut *term, chunk);
            parking_lot::MutexGuard::unlock_fair(term);
        }
        self.publish_deadline(&parser);
    }

    /// Commit bytes still parked in a synchronized update. Returns whether
    /// anything was committed so the caller can request a final redraw.
    pub(crate) fn finish(&self) -> bool {
        let mut parser = self.processor.lock();
        let pending =
            parser.sync_timeout().sync_timeout().is_some() || parser.sync_bytes_count() > 0;
        if pending {
            let mut term = self.term.lock();
            parser.stop_sync(&mut *term);
            parking_lot::MutexGuard::unlock_fair(term);
        }
        self.publish_deadline(&parser);
        pending
    }

    /// Publish vte's deadline while holding the parser lock so the timer
    /// and reader can never disagree on which deadline is current.
    fn publish_deadline(&self, parser: &Processor) {
        let deadline = parser.sync_timeout().sync_timeout();
        let _ = self.deadlines.send_if_modified(|slot| {
            if *slot == deadline {
                false
            } else {
                *slot = deadline;
                true
            }
        });
    }
}

struct SyncTimer<F> {
    deadlines: watch::Receiver<Option<Instant>>,
    processor: Arc<Mutex<Processor>>,
    term: Arc<Mutex<Term<Listener>>>,
    on_timeout: Arc<F>,
}

impl<F> SyncTimer<F>
where
    F: Fn() + Send + Sync + 'static,
{
    async fn run(mut self) {
        loop {
            let current = *self.deadlines.borrow_and_update();
            let Some(deadline) = current else {
                if self.deadlines.changed().await.is_err() {
                    return;
                }
                continue;
            };

            if deadline > Instant::now() {
                let sleep = sleep_until(TokioInstant::from_std(deadline));
                tokio::pin!(sleep);
                tokio::select! {
                    biased;
                    changed = self.deadlines.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        continue;
                    }
                    () = &mut sleep => {}
                }
            }

            // Expiry work (re-check, drain a possibly multi-MiB buffer,
            // callback) must not block a Tokio worker.
            let outcome = {
                let processor = Arc::clone(&self.processor);
                let term = Arc::clone(&self.term);
                let on_timeout = Arc::clone(&self.on_timeout);
                tokio::task::spawn_blocking(move || {
                    commit_if_expired(&processor, &term, &*on_timeout)
                })
                .await
            };
            match outcome {
                Ok(true) => {
                    // The watch still holds the stale deadline; wait for
                    // the reader's next value or the driver to be dropped.
                    if self.deadlines.changed().await.is_err() {
                        return;
                    }
                }
                // Extended, completed, or a premature wake: the reader
                // publishes under the parser lock, so re-read the watch.
                Ok(false) => continue,
                // Blocking pool gone or the commit panicked.
                Err(_) => return,
            }
        }
    }
}

/// Re-check under the parser lock, then drain and terminate the
/// synchronized update. Parser → grid order; both locks are dropped before
/// the callback runs.
fn commit_if_expired<F>(
    processor: &Mutex<Processor>,
    term: &Mutex<Term<Listener>>,
    on_timeout: &F,
) -> bool
where
    F: Fn(),
{
    {
        let mut parser = processor.lock();
        let Some(deadline) = parser.sync_timeout().sync_timeout() else {
            return false;
        };
        if Instant::now() < deadline {
            return false;
        }
        let mut grid = term.lock();
        parser.stop_sync(&mut *grid);
        parking_lot::MutexGuard::unlock_fair(grid);
    }
    on_timeout();
    true
}
