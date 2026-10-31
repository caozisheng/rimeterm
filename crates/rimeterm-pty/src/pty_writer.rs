//! Non-blocking stdin writer for a native PTY.
//!
//! Writing to a PTY (ConPTY on Windows in particular) is a synchronous OS
//! call that can block when the child stops draining its input pipe. The
//! TUI thread must never sit in that call: a background agent that pauses
//! its reads would otherwise freeze the whole UI.
//!
//! [`PtyWriter`] is a [`Write`] adapter with a FIFO queue, the same shape
//! as the remote stdin channel and alacritty's input path. A dedicated OS
//! thread per session owns the real PTY writer and performs `write_all` +
//! `flush` in order. The queue only carries user input and the read loop's
//! small DA/DSR replies — never PTY output — so an unbounded queue stays
//! small for interactive sessions.
//!
//! The worker is detached: dropping the adapter closes the queue, the
//! worker drains what was already accepted, flushes, releases the raw
//! writer, and exits. `Drop` never joins the worker, so tearing a session
//! down cannot hang on a wedged PTY write.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

struct State {
    pending: VecDeque<Vec<u8>>,
    closed: bool,
    /// Kind of the first worker write error; later writes fail fast with
    /// the same kind rather than pretending to enqueue.
    failure: Option<io::ErrorKind>,
}

/// Queue-backed PTY stdin writer. See the module docs.
pub(crate) struct PtyWriter {
    shared: Arc<Shared>,
}

impl PtyWriter {
    /// Wrap a raw PTY writer and start the per-session writer thread.
    pub(crate) fn spawn(raw: Box<dyn Write + Send>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: VecDeque::new(),
                closed: false,
                failure: None,
            }),
            cv: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || worker_loop(raw, &worker_shared))?;
        Ok(Self { shared })
    }
}

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut st = self.shared.state.lock();
        if let Some(kind) = st.failure {
            return Err(io::Error::new(kind, "pty writer worker failed"));
        }
        if st.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pty writer queue shut down",
            ));
        }
        st.pending.push_back(buf.to_vec());
        drop(st);
        self.shared.cv.notify_one();
        Ok(buf.len())
    }

    /// Flushing is the worker's job (it flushes after every buffer);
    /// the caller-side flush is a no-op so it can never block the UI.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PtyWriter {
    fn drop(&mut self) {
        let mut st = self.shared.state.lock();
        st.closed = true;
        drop(st);
        self.shared.cv.notify_all();
    }
}

/// Worker body. Owns `raw` for its whole life; exits after the queue is
/// closed and drained, or on the first write error.
fn worker_loop(mut raw: Box<dyn Write + Send>, shared: &Shared) {
    let mut failed = false;
    loop {
        let next = {
            let mut st = shared.state.lock();
            loop {
                if let Some(buf) = st.pending.pop_front() {
                    break Some(buf);
                }
                if st.closed {
                    break None;
                }
                shared.cv.wait(&mut st);
            }
        };
        let Some(buf) = next else {
            break;
        };
        let result = raw.write_all(&buf).and_then(|()| raw.flush());
        if let Err(e) = result {
            failed = true;
            let mut st = shared.state.lock();
            let dropped = st.pending.len();
            st.failure = Some(e.kind());
            st.pending.clear();
            st.closed = true;
            drop(st);
            shared.cv.notify_all();
            tracing::warn!(
                error = %e,
                dropped = dropped,
                "pty writer failed; discarding queued stdin"
            );
            break;
        }
    }
    if !failed {
        let _ = raw.flush();
    }
    let mut st = shared.state.lock();
    st.closed = true;
    shared.cv.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Write sink whose `write` blocks on a gate until the test opens it.
    /// `entered` is signalled as soon as the worker is inside `write`.
    struct BlockingWriter {
        entered: Arc<(Mutex<bool>, Condvar)>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        sink: Arc<Mutex<Vec<u8>>>,
        flush_count: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    struct BlockingHandle {
        entered: Arc<(Mutex<bool>, Condvar)>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        sink: Arc<Mutex<Vec<u8>>>,
        flush_count: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl BlockingHandle {
        fn new() -> (Self, BlockingWriter) {
            let entered = Arc::new((Mutex::new(false), Condvar::new()));
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            let sink = Arc::new(Mutex::new(Vec::new()));
            let flush_count = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicBool::new(false));
            let writer = BlockingWriter {
                entered: Arc::clone(&entered),
                gate: Arc::clone(&gate),
                sink: Arc::clone(&sink),
                flush_count: Arc::clone(&flush_count),
                dropped: Arc::clone(&dropped),
            };
            (
                Self {
                    entered,
                    gate,
                    sink,
                    flush_count,
                    dropped,
                },
                writer,
            )
        }

        fn wait_entered(&self, timeout: Duration) -> bool {
            let (lock, cv) = &*self.entered;
            let mut entered = lock.lock();
            let deadline = Instant::now() + timeout;
            while !*entered {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                if cv.wait_for(&mut entered, left).timed_out() {
                    return !*entered;
                }
            }
            true
        }

        fn release(&self) {
            let (lock, cv) = &*self.gate;
            *lock.lock() = true;
            cv.notify_all();
        }

        fn bytes(&self) -> Vec<u8> {
            self.sink.lock().clone()
        }

        fn flushes(&self) -> usize {
            self.flush_count.load(Ordering::Acquire)
        }

        fn dropped(&self) -> bool {
            self.dropped.load(Ordering::Acquire)
        }
    }

    impl Write for BlockingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let (lock, cv) = &*self.entered;
            *lock.lock() = true;
            cv.notify_all();
            let (lock, cv) = &*self.gate;
            let mut open = lock.lock();
            while !*open {
                // Watchdog: if a test panics before releasing the gate,
                // never leave this worker wedged forever.
                if cv.wait_for(&mut open, Duration::from_secs(30)).timed_out() {
                    break;
                }
            }
            drop(open);
            self.sink.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    impl Drop for BlockingWriter {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

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

    #[test]
    fn enqueue_returns_before_a_blocked_write_completes() {
        let (handle, writer) = BlockingHandle::new();
        let pty = PtyWriter::spawn(Box::new(writer)).expect("worker");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            let mut pty = pty;
            let result = pty.write_all(b"hello");
            let _ = done_tx.send(result);
            pty
        });
        let returned = done_rx.recv_timeout(Duration::from_millis(500));
        let worker_started = handle.wait_entered(Duration::from_secs(2));
        if returned.is_err() || !worker_started {
            // Regression guard: unblock a synchronous implementation so
            // the test fails instead of hanging forever.
            handle.release();
        }
        let pty = caller.join().expect("caller thread");
        assert!(worker_started, "worker should pick up the queued buffer");
        assert!(
            returned.is_ok(),
            "write must return after enqueueing, not wait for the OS write"
        );
        returned.unwrap().expect("enqueue while worker blocked");
        handle.release();
        assert!(wait_for(
            || handle.bytes() == b"hello",
            Duration::from_secs(2)
        ));
        drop(pty);
        assert!(wait_for(|| handle.flushes() >= 1, Duration::from_secs(2)));
        assert!(wait_for(|| handle.dropped(), Duration::from_secs(2),));
        assert!(handle.flushes() >= 1, "worker flushes after each buffer");
    }

    #[test]
    fn writes_keep_fifo_order() {
        let (handle, writer) = BlockingHandle::new();
        let mut pty = PtyWriter::spawn(Box::new(writer)).expect("worker");
        pty.write_all(b"first").unwrap();
        pty.write_all(b"-second").unwrap();
        pty.write_all(b"-third").unwrap();
        assert!(handle.wait_entered(Duration::from_secs(2)));
        handle.release();
        assert!(wait_for(
            || handle.bytes() == b"first-second-third",
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn dropping_writer_drains_queue_and_exits_worker() {
        let (handle, writer) = BlockingHandle::new();
        let mut pty = PtyWriter::spawn(Box::new(writer)).expect("worker");
        pty.write_all(b"queued-1").unwrap();
        pty.write_all(b"queued-2").unwrap();
        assert!(handle.wait_entered(Duration::from_secs(2)));
        handle.release();
        drop(pty);
        assert!(
            wait_for(|| handle.dropped(), Duration::from_secs(2)),
            "worker must drain and release the raw writer after the adapter drops"
        );
        assert_eq!(handle.bytes(), b"queued-1queued-2");
    }

    #[test]
    fn worker_records_write_error_and_rejects_further_input() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut pty = PtyWriter::spawn(Box::new(FailingWriter)).expect("worker");
        // The worker may fail before or after this first enqueue; either
        // is fine as long as later writes report the failure.
        let _ = pty.write(b"boom");
        assert!(
            wait_for(|| pty.write(b"next").is_err(), Duration::from_secs(2)),
            "later writes must report the worker failure"
        );
    }
}
