//! Per-session control workers for a native PTY.
//!
//! `Session::resize` and `Session::kill` used to run the underlying OS
//! calls on whichever thread called them — normally the TUI thread.
//! Windows' `ResizePseudoConsole` is a synchronous, comparatively
//! expensive call, and tree-kill spawns and waits for `taskkill`; both
//! can stall a frame. This module moves that work to dedicated OS
//! threads fed by channels:
//!
//! - **resize** — a single worker per session with a one-slot "latest
//!   size" mailbox. Resize storms (window drags) coalesce: every size
//!   the user dragged through between two syscalls is superseded by the
//!   newest one, so the worker never thrashes the PTY.
//! - **kill** — its own worker and channel, deliberately separate from
//!   resize. A `ResizePseudoConsole` that blocks on a wedged ConPTY must
//!   never delay termination.
//!
//! Both enqueue operations are lock-and-notify only: no OS call runs on
//! the caller's thread. Workers are detached; dropping the handle closes
//! their channels and lets them exit after any in-flight call returns.

use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

/// Handle onto a session's resize + kill workers.
pub(crate) struct ControlWorker {
    resize: Arc<ResizeShared>,
    kill_tx: tokio::sync::mpsc::Sender<()>,
}

impl ControlWorker {
    /// Start the resize and kill workers.
    ///
    /// `resize` runs for every coalesced size request; `kill` runs at
    /// most once when killed. Both must not hold the caller-facing
    /// session lock.
    pub(crate) fn spawn<R, K>(resize: R, kill: K) -> std::io::Result<Self>
    where
        R: FnMut(u16, u16) + Send + 'static,
        K: FnOnce() + Send + 'static,
    {
        let resize_shared = Arc::new(ResizeShared {
            state: Mutex::new(ResizeState {
                size: None,
                closed: false,
            }),
            cv: Condvar::new(),
        });

        let resize_thread_shared = Arc::clone(&resize_shared);
        let resize_handle = std::thread::Builder::new()
            .name("pty-resize".into())
            .spawn(move || resize_loop(&resize_thread_shared, resize))?;

        let (kill_tx, kill_rx) = tokio::sync::mpsc::channel::<()>(1);
        let kill_handle = match std::thread::Builder::new()
            .name("pty-kill".into())
            .spawn(move || kill_loop(kill_rx, kill))
        {
            Ok(handle) => handle,
            Err(e) => {
                // Do not leak the resize worker if the kill worker could
                // not be started; close it and report the failure.
                resize_shared.close();
                drop(resize_handle);
                return Err(e);
            }
        };

        // Detach: workers exit on their own once the handle is dropped
        // and they finish the in-flight call. Joining, especially for a
        // blocked resize, would reintroduce the UI stall we just removed.
        drop(resize_handle);
        drop(kill_handle);

        Ok(Self {
            resize: resize_shared,
            kill_tx,
        })
    }

    /// Queue a resize. Only the newest size survives if the worker is
    /// busy. Returns `false` once the worker has shut down.
    pub(crate) fn resize(&self, cols: u16, rows: u16) -> bool {
        self.resize.enqueue(cols, rows)
    }

    /// Request termination. Never blocks on resize or write activity.
    /// The channel holds at most one request; a second call while one is
    /// pending is redundant and dropped.
    pub(crate) fn kill(&self) {
        let _ = self.kill_tx.try_send(());
    }
}

impl Drop for ControlWorker {
    fn drop(&mut self) {
        self.resize.close();
        // Dropping `kill_tx` closes the channel; the kill worker exits
        // without killing, which is the correct teardown semantics:
        // teardown that wants a kill calls `kill()` first.
    }
}

struct ResizeShared {
    state: Mutex<ResizeState>,
    cv: Condvar,
}

struct ResizeState {
    /// Latest requested size; older requests are overwritten on purpose.
    size: Option<(u16, u16)>,
    closed: bool,
}

impl ResizeShared {
    fn enqueue(&self, cols: u16, rows: u16) -> bool {
        let mut st = self.state.lock();
        if st.closed {
            return false;
        }
        st.size = Some((cols, rows));
        drop(st);
        self.cv.notify_one();
        true
    }

    fn close(&self) {
        let mut st = self.state.lock();
        st.closed = true;
        drop(st);
        self.cv.notify_all();
    }
}

fn resize_loop<R>(shared: &ResizeShared, mut resize: R)
where
    R: FnMut(u16, u16),
{
    loop {
        let next = {
            let mut st = shared.state.lock();
            loop {
                if let Some(size) = st.size.take() {
                    break Some(size);
                }
                if st.closed {
                    break None;
                }
                shared.cv.wait(&mut st);
            }
        };
        let Some((cols, rows)) = next else {
            return;
        };
        resize(cols, rows);
    }
}

fn kill_loop<K>(mut kill_rx: tokio::sync::mpsc::Receiver<()>, kill: K)
where
    K: FnOnce(),
{
    // `blocking_recv` is legal here: this is a plain OS thread, not a
    // tokio runtime worker.
    if kill_rx.blocking_recv().is_some() {
        kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn wait_for(mut pred: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        pred()
    }

    /// Resize closure that blocks the first call until released and
    /// records every size it actually receives.
    struct BlockingResize {
        entered: Arc<(Mutex<bool>, Condvar)>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        calls: Arc<Mutex<Vec<(u16, u16)>>>,
    }

    impl BlockingResize {
        fn new() -> (Self, impl FnMut(u16, u16) + Send + 'static) {
            let entered = Arc::new((Mutex::new(false), Condvar::new()));
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            let calls = Arc::new(Mutex::new(Vec::new()));
            let entered_for_closure = Arc::clone(&entered);
            let gate_for_closure = Arc::clone(&gate);
            let calls_for_closure = Arc::clone(&calls);
            let resize = move |cols: u16, rows: u16| {
                let (lock, cv) = &*entered_for_closure;
                *lock.lock() = true;
                cv.notify_all();
                let (lock, cv) = &*gate_for_closure;
                let mut open = lock.lock();
                while !*open {
                    // Watchdog: never wedge this worker forever if a
                    // test panics before releasing the gate.
                    if cv.wait_for(&mut open, Duration::from_secs(30)).timed_out() {
                        break;
                    }
                }
                drop(open);
                calls_for_closure.lock().push((cols, rows));
            };
            (
                Self {
                    entered,
                    gate,
                    calls,
                },
                resize,
            )
        }

        fn wait_entered(&self, timeout: Duration) -> bool {
            let (lock, cv) = &*self.entered;
            let mut entered = lock.lock();
            let deadline = Instant::now() + timeout;
            while !*entered {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || cv.wait_for(&mut entered, left).timed_out() {
                    return *entered;
                }
            }
            true
        }

        fn release(&self) {
            let (lock, cv) = &*self.gate;
            *lock.lock() = true;
            cv.notify_all();
        }

        fn calls(&self) -> Vec<(u16, u16)> {
            self.calls.lock().clone()
        }
    }

    #[test]
    fn resize_enqueues_without_waiting_and_coalesces_to_latest() {
        let (blocking, resize) = BlockingResize::new();
        let worker = Arc::new(ControlWorker::spawn(resize, || {}).expect("control workers start"));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_for_call = Arc::clone(&worker);
        let caller = std::thread::spawn(move || {
            let _ = done_tx.send(worker_for_call.resize(10, 10));
        });
        let returned = done_rx.recv_timeout(Duration::from_millis(500));
        let entered = blocking.wait_entered(Duration::from_secs(2));
        if returned.is_err() || !entered {
            // Unblock a synchronous implementation so a regression fails
            // rather than hanging the test.
            blocking.release();
        }
        caller.join().expect("resize caller thread");
        assert!(entered, "resize worker should enter the (blocked) OS call");
        assert!(
            returned.expect("resize must enqueue without waiting"),
            "resize must enqueue, not run the OS call inline"
        );

        // While the first syscall is blocked, these must collapse into
        // one pending size.
        assert!(worker.resize(20, 20));
        assert!(worker.resize(30, 30));
        assert!(worker.resize(40, 40));
        blocking.release();

        assert!(wait_for(
            || blocking.calls().len() == 2,
            Duration::from_secs(2)
        ));
        let calls = blocking.calls();
        assert_eq!(calls[0], (10, 10));
        assert_eq!(
            calls[1],
            (40, 40),
            "intermediate sizes must be superseded, not replayed: {calls:?}"
        );
    }

    #[test]
    fn kill_runs_while_resize_is_blocked() {
        let (blocking, resize) = BlockingResize::new();
        let killed = Arc::new(AtomicBool::new(false));
        let killed_for_closure = Arc::clone(&killed);
        let worker = Arc::new(
            ControlWorker::spawn(resize, move || {
                killed_for_closure.store(true, Ordering::Release);
            })
            .expect("control workers start"),
        );

        let (resize_tx, resize_rx) = std::sync::mpsc::channel();
        let worker_for_resize = Arc::clone(&worker);
        let resize_caller = std::thread::spawn(move || {
            let _ = resize_tx.send(worker_for_resize.resize(80, 24));
        });
        let enqueued = resize_rx.recv_timeout(Duration::from_millis(500));
        let entered = blocking.wait_entered(Duration::from_secs(2));
        if enqueued.is_err() || !entered {
            blocking.release();
        }
        resize_caller.join().expect("resize caller thread");
        assert!(
            enqueued.expect("resize must enqueue"),
            "resize must not run the OS call inline"
        );
        assert!(entered, "resize worker should be blocked in the OS call");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_for_call = Arc::clone(&worker);
        let caller = std::thread::spawn(move || {
            worker_for_call.kill();
            let _ = done_tx.send(());
        });
        let returned = done_rx.recv_timeout(Duration::from_millis(500));
        if returned.is_err() {
            // Unblock the resize worker so a regression cannot hang.
            blocking.release();
        }
        caller.join().expect("kill caller thread");
        assert!(returned.is_ok(), "kill must not wait for a blocked resize");
        assert!(
            wait_for(|| killed.load(Ordering::Acquire), Duration::from_secs(2)),
            "kill worker must run independently of the blocked resize worker"
        );
        blocking.release();
    }

    #[test]
    fn kill_fires_once_and_drop_does_not_hang() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = Arc::clone(&calls);
        let worker = ControlWorker::spawn(
            |_, _| {},
            move || {
                calls_for_closure.fetch_add(1, Ordering::AcqRel);
            },
        )
        .expect("control workers start");
        worker.kill();
        worker.kill();
        assert!(wait_for(
            || calls.load(Ordering::Acquire) == 1,
            Duration::from_secs(2)
        ));
        // The second request finds a closed kill channel; dropping must
        // return immediately.
        let started = Instant::now();
        drop(worker);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn resize_worker_drops_captured_resources_off_the_caller_thread() {
        struct DropSpy {
            dropped: Arc<AtomicBool>,
            thread: Arc<Mutex<Option<std::thread::ThreadId>>>,
        }
        impl Drop for DropSpy {
            fn drop(&mut self) {
                *self.thread.lock() = Some(std::thread::current().id());
                self.dropped.store(true, Ordering::Release);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let drop_thread = Arc::new(Mutex::new(None));
        let spy = DropSpy {
            dropped: Arc::clone(&dropped),
            thread: Arc::clone(&drop_thread),
        };
        // The closure owns its captures, exactly like the production
        // resize closure owns the last `MasterPty` Arc.
        let worker = ControlWorker::spawn(
            move |_, _| {
                let _ = &spy;
            },
            || {},
        )
        .expect("control workers start");
        let caller = std::thread::current().id();
        drop(worker);
        assert!(
            wait_for(|| dropped.load(Ordering::Acquire), Duration::from_secs(2)),
            "resize worker must release its captures after the Session drops"
        );
        let drop_thread = drop_thread.lock().clone();
        assert_ne!(
            drop_thread.expect("capture must be dropped"),
            caller,
            "the last MasterPty-like resource must be dropped on the resize worker, \
             not on the thread that dropped the Session"
        );
    }
}
