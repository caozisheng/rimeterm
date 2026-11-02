//! Latest-wins frame presentation.
//!
//! The UI thread only composes frames into an in-memory [`ratatui::backend::TestBackend`];
//! this module owns the one thread allowed to write to the application's stdout. It keeps a
//! single pending snapshot, serialises complete frames inside a synchronized-update block, and
//! diffs every frame against the buffer that was actually presented — never against a submitted
//! frame that was coalesced away.
//!
//! Submit never waits for the writer: the mailbox lock is held only long enough to replace the
//! pending snapshot, and the worker releases the same lock before touching stdout.

use std::io::{self, BufWriter, Write};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossterm::cursor::{Hide, SetCursorStyle, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    BeginSynchronizedUpdate, Clear as ClearScreen, ClearType, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use parking_lot::{Condvar, Mutex, MutexGuard};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Size of the presenter's stdout buffer. Large enough that a typical frame reaches the OS in a
/// single write while still bounding memory for very large terminals.
pub(crate) const STDOUT_BUFFER_BYTES: usize = 128 * 1024;

/// One complete frame as handed from the UI thread to the presenter.
struct PendingFrame {
    buffer: Buffer,
    cursor: Option<(u16, u16)>,
}

#[derive(Default)]
struct State {
    /// At most one frame is queued; a newer frame replaces it wholesale.
    pending: Option<PendingFrame>,
    /// A buffer owned by the presenter that the UI may refill, avoiding a per-frame allocation.
    recycle: Option<Buffer>,
    /// First write error, surfaced to the next UI draw.
    error: Option<io::Error>,
    /// Set while holding this mutex so a waiter can never miss the wake-up.
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock()
    }

    fn record_error(&self, error: io::Error) {
        let mut state = self.lock();
        if state.error.is_none() {
            state.error = Some(error);
        }
    }
}

/// Handle to the dedicated stdout worker.
///
/// The worker performs terminal setup (alt screen, mouse capture, cursor shape) before its first
/// frame and restores those modes when it shuts down. All stdout I/O happens on that thread.
pub(crate) struct Presenter {
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Presenter {
    /// Spawn a presenter that writes to `writer`. Production passes a `BufWriter<Stdout>`; tests
    /// pass a virtual terminal writer.
    pub(crate) fn spawn<W>(writer: W) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("rimeterm-presenter".to_owned())
            .spawn(move || run_worker(&worker_shared, writer))?;
        Ok(Self {
            shared,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Spawn the production presenter over stdout with the configured buffer capacity.
    pub(crate) fn start() -> io::Result<Self> {
        Self::spawn(BufWriter::with_capacity(STDOUT_BUFFER_BYTES, io::stdout()))
    }

    /// Queue the latest complete frame. Never waits for the writer; a frame already queued is
    /// overwritten so the worker always presents the newest state.
    pub(crate) fn submit(&self, buffer: &Buffer, cursor: Option<(u16, u16)>) {
        let mut state = self.shared.lock();
        if state.error.is_some() || state.shutdown {
            return;
        }
        match state.pending.as_mut() {
            Some(pending) => {
                // Reuse the queued buffer's allocation instead of replacing the Vec.
                pending.buffer.area = buffer.area;
                pending.buffer.content.clone_from(&buffer.content);
                pending.cursor = cursor;
            }
            None => {
                let mut frame = state
                    .recycle
                    .take()
                    .unwrap_or_else(|| Buffer::empty(buffer.area));
                frame.area = buffer.area;
                frame.content.clone_from(&buffer.content);
                state.pending = Some(PendingFrame {
                    buffer: frame,
                    cursor,
                });
            }
        }
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Return the first output failure. The error is cleared after being reported so the caller
    /// sees it exactly once; the worker has already stopped writing.
    pub(crate) fn take_error(&self) -> Option<io::Error> {
        self.shared.lock().error.take()
    }

    /// Signal the worker to restore the terminal and wait for it to finish. Idempotent and called
    /// from [`super::terminal::TerminalGuard`]'s drop so no writes can race raw-mode teardown.
    pub(crate) fn shutdown(&self) {
        let handle = self.handle.lock().take();
        if let Some(handle) = handle {
            // Store under the same mutex the worker checks before waiting. Otherwise the worker
            // could observe `shutdown == false`, release the lock, and miss the notify before
            // parking, leaving this join blocked forever.
            {
                let mut state = self.shared.lock();
                state.shutdown = true;
            }
            self.shared.wake.notify_one();
            let _ = handle.join();
        }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn take_next(shared: &Shared) -> Option<PendingFrame> {
    let mut state = shared.lock();
    loop {
        if state.shutdown {
            return None;
        }
        if let Some(frame) = state.pending.take() {
            return Some(frame);
        }
        shared.wake.wait(&mut state);
    }
}

fn run_worker<W>(shared: &Shared, mut writer: W)
where
    W: Write,
{
    if let Err(error) = write_setup(&mut writer) {
        shared.record_error(error);
        // Best effort: if setup failed part-way the alt screen may be active.
        let _ = write_restore(&mut writer);
        return;
    }

    let mut scratch = Vec::new();
    let mut presented = Buffer::empty(Rect::ZERO);

    while let Some(frame) = take_next(shared) {
        let resized = presented.area != frame.buffer.area;
        if resized {
            // Start from a blank buffer of the new area so the diff repaints every cell and the
            // explicit clear removes anything left by the previous geometry.
            presented = Buffer::empty(frame.buffer.area);
        }
        let result = compose_frame(
            &mut scratch,
            &presented,
            &frame.buffer,
            frame.cursor,
            resized,
        )
        .and_then(|()| {
            writer.write_all(&scratch)?;
            writer.flush()
        });
        match result {
            Ok(()) => {
                let previous = std::mem::replace(&mut presented, frame.buffer);
                let mut state = shared.lock();
                if state.recycle.is_none() {
                    state.recycle = Some(previous);
                }
            }
            Err(error) => {
                shared.record_error(error);
                break;
            }
        }
    }

    // Restore cursor shape, alt screen, and mouse capture before the worker exits. The join in
    // `shutdown` guarantees raw mode is not disabled until these bytes have been flushed.
    let _ = write_restore(&mut writer);
}

/// Serialize one complete frame into `scratch`: synchronized-update begin, hide, diff, cursor,
/// synchronized-update end. The caller writes the whole buffer in one call so an outer terminal
/// never observes a frame boundary in the middle of a repaint.
fn compose_frame(
    scratch: &mut Vec<u8>,
    presented: &Buffer,
    next: &Buffer,
    cursor: Option<(u16, u16)>,
    clear: bool,
) -> io::Result<()> {
    scratch.clear();
    let mut backend = CrosstermBackend::new(&mut *scratch);
    execute!(backend, BeginSynchronizedUpdate, Hide)?;
    if clear {
        execute!(backend, ClearScreen(ClearType::All))?;
    }
    backend.draw(presented.diff_iter(next))?;
    if let Some((x, y)) = cursor {
        backend.set_cursor_position((x, y))?;
        backend.show_cursor()?;
    }
    execute!(backend, EndSynchronizedUpdate)?;
    Ok(())
}

fn write_setup<W: Write>(writer: &mut W) -> io::Result<()> {
    let mut bytes = Vec::new();
    {
        let mut backend = CrosstermBackend::new(&mut bytes);
        // BlinkingBlock: DEC "\x1b[1 q". Alt-screen entry can reset the caret to steady on some
        // terminals (Windows Terminal in particular), so request the blink explicitly.
        execute!(
            backend,
            EnterAlternateScreen,
            EnableMouseCapture,
            SetCursorStyle::BlinkingBlock,
        )?;
    }
    writer.write_all(&bytes)?;
    writer.flush()
}

fn write_restore<W: Write>(writer: &mut W) -> io::Result<()> {
    let mut bytes = Vec::new();
    {
        let mut backend = CrosstermBackend::new(&mut bytes);
        // If a frame write failed mid-update the outer terminal may still be inside a synchronized
        // update; close it first, then reset the caret shape, leave the alt screen, and release
        // mouse capture. All of it is best effort.
        execute!(
            backend,
            EndSynchronizedUpdate,
            Show,
            SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen,
            DisableMouseCapture,
        )?;
    }
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::Presenter;

    use std::io::{self, Write};
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use parking_lot::{Condvar as ParkingCondvar, Mutex};

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::{Config, Term, TermMode};
    use alacritty_terminal::vte::ansi::Processor;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[derive(Clone, Copy)]
    struct Size {
        cols: usize,
        lines: usize,
    }

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.lines
        }

        fn screen_lines(&self) -> usize {
            self.lines
        }

        fn columns(&self) -> usize {
            self.cols
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ScreenState {
        text: Vec<String>,
        cursor_visible: bool,
        cursor: (u16, u16),
    }

    struct Host {
        size: Size,
        term: Term<VoidListener>,
        parser: Processor,
        writes: usize,
        states: Vec<ScreenState>,
    }

    impl Host {
        fn new(cols: u16, lines: u16) -> Self {
            let size = Size {
                cols: usize::from(cols),
                lines: usize::from(lines),
            };
            Self {
                size,
                term: Term::new(Config::default(), &size, VoidListener),
                parser: Processor::new(),
                writes: 0,
                states: Vec::new(),
            }
        }

        fn resize(&mut self, cols: u16, lines: u16) {
            let size = Size {
                cols: usize::from(cols),
                lines: usize::from(lines),
            };
            self.term.resize(size);
            self.size = size;
        }

        fn snapshot(&self) -> ScreenState {
            let text: Vec<String> = (0..self.size.lines)
                .map(|row| {
                    (0..self.size.cols)
                        .map(|col| self.term.grid()[Line(row as i32)][Column(col)].c)
                        .collect()
                })
                .collect();
            let point = self.term.grid().cursor.point;
            ScreenState {
                text,
                cursor_visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
                cursor: (point.column.0 as u16, point.line.0.max(0) as u16),
            }
        }
    }

    struct HostShared {
        host: Mutex<Host>,
        written: ParkingCondvar,
    }

    impl HostShared {
        fn new(cols: u16, lines: u16) -> Self {
            Self {
                host: Mutex::new(Host::new(cols, lines)),
                written: ParkingCondvar::new(),
            }
        }

        fn resize(&self, cols: u16, lines: u16) {
            self.host.lock().resize(cols, lines);
        }

        fn wait_for_writes(&self, writes: usize) {
            let mut host = self.host.lock();
            let deadline = Instant::now() + Duration::from_secs(5);
            while host.writes < writes {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    panic!(
                        "timed out waiting for {writes} writes (saw {})",
                        host.writes
                    );
                }
                self.written.wait_for(&mut host, remaining);
            }
        }
    }

    #[derive(Clone)]
    struct HostWriter {
        shared: Arc<HostShared>,
    }

    impl Write for HostWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut host = self.shared.host.lock();
            {
                let Host { term, parser, .. } = &mut *host;
                parser.advance(term, bytes);
            }
            host.writes += 1;
            let state = host.snapshot();
            host.states.push(state);
            drop(host);
            self.shared.written.notify_all();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn frame(lines: &[&str], cursor: Option<(u16, u16)>) -> (Buffer, Option<(u16, u16)>) {
        let width = lines.iter().map(|line| line.len()).max().unwrap_or(0) as u16;
        let height = lines.len() as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        for (y, line) in lines.iter().enumerate() {
            for (x, symbol) in line.chars().enumerate() {
                buffer[(x as u16, y as u16)].set_char(symbol);
            }
        }
        (buffer, cursor)
    }

    fn expected(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    /// Writer that blocks every write after `block_after`, emulating a slow outer terminal.
    struct Gate {
        state: Mutex<(bool, bool)>, // (entered, open)
        changed: ParkingCondvar,
    }

    impl Gate {
        fn new() -> Self {
            Self {
                state: Mutex::new((false, false)),
                changed: ParkingCondvar::new(),
            }
        }

        fn wait_entered(&self) {
            let mut state = self.state.lock();
            while !state.0 {
                self.changed.wait(&mut state);
            }
        }

        fn open(&self) {
            let mut state = self.state.lock();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    struct GateWriter {
        inner: HostWriter,
        gate: Arc<Gate>,
        block_after: usize,
        calls: usize,
    }

    impl Write for GateWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls > self.block_after {
                let mut state = self.gate.state.lock();
                state.0 = true;
                self.gate.changed.notify_all();
                while !state.1 {
                    self.gate.changed.wait(&mut state);
                }
            }
            self.inner.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn host_never_observes_half_a_frame_or_a_transient_caret() {
        let shared = Arc::new(HostShared::new(4, 2));
        let presenter = Presenter::spawn(HostWriter {
            shared: Arc::clone(&shared),
        })
        .unwrap();

        let (first, cursor) = frame(&["AB", "CD"], Some((1, 0)));
        presenter.submit(&first, cursor);
        shared.wait_for_writes(2);

        let (second, cursor) = frame(&["EF", "GH"], Some((0, 1)));
        presenter.submit(&second, cursor);
        shared.wait_for_writes(3);

        {
            let host = shared.host.lock();
            assert_eq!(
                host.states[1].text,
                vec!["AB  ".to_owned(), "CD  ".to_owned()]
            );
            assert_eq!(host.states[1].cursor, (1, 0));
            assert!(host.states[1].cursor_visible);

            assert_eq!(
                host.states[2].text,
                vec!["EF  ".to_owned(), "GH  ".to_owned()]
            );
            assert_eq!(host.states[2].cursor, (0, 1));
            assert!(host.states[2].cursor_visible);

            // Every write after setup is exactly one submitted frame: no partial text and no
            // caret state belonging to an in-between repaint.
            for state in &host.states[1..3] {
                let complete = state.text == vec!["AB  ".to_owned(), "CD  ".to_owned()]
                    || state.text == vec!["EF  ".to_owned(), "GH  ".to_owned()];
                assert!(complete, "observed a partial frame: {state:?}");
                assert!(state.cursor_visible);
            }
        }

        presenter.shutdown();
    }

    #[test]
    fn blocked_writer_keeps_submit_non_blocking_and_latest_frame_wins() {
        let shared = Arc::new(HostShared::new(4, 2));
        let gate = Arc::new(Gate::new());
        let presenter = Arc::new(
            Presenter::spawn(GateWriter {
                inner: HostWriter {
                    shared: Arc::clone(&shared),
                },
                gate: Arc::clone(&gate),
                block_after: 1,
                calls: 0,
            })
            .unwrap(),
        );

        let (first, cursor) = frame(&["AAAA", "AAAA"], Some((0, 0)));
        presenter.submit(&first, cursor);
        gate.wait_entered();

        let (second, second_cursor) = frame(&["BBBB", "BBBB"], Some((1, 0)));
        let (third, third_cursor) = frame(&["CCCC", "CCCC"], Some((2, 0)));
        let (done, finished) = mpsc::channel();
        let submitter = {
            let presenter = Arc::clone(&presenter);
            thread::spawn(move || {
                presenter.submit(&second, second_cursor);
                presenter.submit(&third, third_cursor);
                done.send(()).unwrap();
            })
        };
        finished
            .recv_timeout(Duration::from_secs(2))
            .expect("submit must return while the writer is blocked");
        submitter.join().unwrap();

        gate.open();
        shared.wait_for_writes(3);

        {
            let host = shared.host.lock();
            // The blocked frame still reached the virtual terminal, then the newest queued
            // frame replaced the coalesced middle frame.
            assert_eq!(host.states[1].text, expected(&["AAAA", "AAAA"]));
            assert_eq!(host.states[2].text, expected(&["CCCC", "CCCC"]));
            assert_eq!(host.states[2].cursor, (2, 0));
            assert!(host.states[2].cursor_visible);
            assert!(
                !host
                    .states
                    .iter()
                    .any(|state| state.text == expected(&["BBBB", "BBBB"])),
                "the coalesced frame must not be presented"
            );
            for state in &host.states[1..3] {
                let complete = state.text == expected(&["AAAA", "AAAA"])
                    || state.text == expected(&["CCCC", "CCCC"]);
                assert!(complete, "observed a partial frame: {state:?}");
            }
        }

        presenter.shutdown();
    }

    #[test]
    fn resize_clears_stale_cells_and_presents_the_new_area() {
        let shared = Arc::new(HostShared::new(4, 2));
        let presenter = Presenter::spawn(HostWriter {
            shared: Arc::clone(&shared),
        })
        .unwrap();

        let (large, cursor) = frame(&["ABCD", "EFGH"], Some((3, 1)));
        presenter.submit(&large, cursor);
        shared.wait_for_writes(2);

        // The outer terminal shrinks; the presenter must detect the new area, clear the screen,
        // and repaint the full new frame so no cell from the old geometry survives.
        shared.resize(3, 3);
        let (small, cursor) = frame(&["XYZ", "", "123"], Some((2, 2)));
        presenter.submit(&small, cursor);
        shared.wait_for_writes(3);

        {
            let host = shared.host.lock();
            assert_eq!(host.states[2].text, expected(&["XYZ", "   ", "123"]));
            assert_eq!(host.states[2].cursor, (2, 2));
            assert!(host.states[2].cursor_visible);
        }

        presenter.shutdown();
    }

    struct FailureFlag {
        failed: Mutex<bool>,
        changed: ParkingCondvar,
    }

    impl FailureFlag {
        fn new() -> Self {
            Self {
                failed: Mutex::new(false),
                changed: ParkingCondvar::new(),
            }
        }

        fn set(&self) {
            *self.failed.lock() = true;
            self.changed.notify_all();
        }

        fn wait(&self) {
            let mut failed = self.failed.lock();
            while !*failed {
                self.changed.wait(&mut failed);
            }
        }
    }

    struct FailingWriter {
        inner: HostWriter,
        calls: usize,
        flag: Arc<FailureFlag>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls > 1 {
                self.flag.set();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "outer terminal gone",
                ));
            }
            self.inner.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn write_failure_is_reported_to_the_next_draw() {
        let shared = Arc::new(HostShared::new(4, 2));
        let flag = Arc::new(FailureFlag::new());
        let presenter = Presenter::spawn(FailingWriter {
            inner: HostWriter {
                shared: Arc::clone(&shared),
            },
            calls: 0,
            flag: Arc::clone(&flag),
        })
        .unwrap();

        let (buffer, cursor) = frame(&["AB", "CD"], Some((1, 0)));
        presenter.submit(&buffer, cursor);
        flag.wait();

        let deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            if let Some(error) = presenter.take_error() {
                break error;
            }
            assert!(Instant::now() < deadline, "write error was never reported");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(
            presenter.take_error().is_none(),
            "error must be reported once"
        );

        // Further submissions are ignored; the worker has stopped writing.
        presenter.submit(&buffer, cursor);
        assert_eq!(shared.host.lock().writes, 1);
        presenter.shutdown();
        assert_eq!(shared.host.lock().writes, 1);
    }

    #[test]
    fn shutdown_restores_terminal_modes_and_stops_writing() {
        let shared = Arc::new(HostShared::new(4, 2));
        let presenter = Presenter::spawn(HostWriter {
            shared: Arc::clone(&shared),
        })
        .unwrap();

        let (buffer, cursor) = frame(&["AB", "CD"], Some((1, 0)));
        presenter.submit(&buffer, cursor);
        shared.wait_for_writes(2);

        let writes_before_shutdown = shared.host.lock().writes;
        presenter.shutdown();

        {
            let host = shared.host.lock();
            assert_eq!(
                host.writes,
                writes_before_shutdown + 1,
                "shutdown must emit exactly one restore write"
            );
            assert!(
                !host.term.mode().contains(TermMode::ALT_SCREEN),
                "shutdown must leave the alternate screen"
            );
            assert!(
                host.term.mode().contains(TermMode::SHOW_CURSOR),
                "shutdown must leave the cursor visible"
            );
            let restored = host.states.last().unwrap();
            assert!(restored.cursor_visible);
        }

        // Nothing may be written after the presenter has been joined.
        presenter.submit(&buffer, cursor);
        presenter.shutdown();
        assert_eq!(
            shared.host.lock().writes,
            writes_before_shutdown + 1,
            "no writes are allowed after shutdown"
        );
        assert!(presenter.take_error().is_none());
    }
}
