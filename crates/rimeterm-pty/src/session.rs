//! One PTY [`Session`] = one child process + its master fd + its alacritty
//! terminal grid.
//!
//! C17 migration: swapped the VT parser from `vt100` to `alacritty_terminal`
//! + `vte::ansi::Processor`. Public API of `Session` is preserved except:
//! - `with_grid(&vt100::Parser)` renamed to `with_term(&Term<VoidListener>)`
//!   because the whole grid type changed.
//! - Style extraction moves into the pane provider (which now sees
//!   `alacritty_terminal::term::cell::Cell` instead of `vt100::Cell`).
//!
//! The migration buys us: correct wide-char handling, real alt-screen
//! swap, damage tracking (not wired yet), regex search (parked), and a
//! much larger real-world compliance test corpus. See §17 rationale in
//! docs/rimeterm-overall-design.md.

use parking_lot::Mutex;
use std::io::Write;
use std::sync::Arc;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Which PTY backend to request. On Windows we hardcode ConPTY (§6.2 table).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum PtyBackend {
    #[default]
    Native,
}

/// Everything the caller specifies to spawn a session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Executable to spawn (typically resolved by [`crate::shell_detect`]).
    pub program: std::path::PathBuf,
    /// Extra command-line args. v0.1 empty for interactive shells.
    pub args: Vec<String>,
    /// Working directory. `None` = inherit from rimeterm process.
    pub cwd: Option<std::path::PathBuf>,
    /// Environment additions (merged over the inherited env).
    pub env: Vec<(String, String)>,
    /// Initial PTY size. Caller SHOULD call [`Session::resize`] before any input.
    pub cols: u16,
    pub rows: u16,
    /// Which backend flavor to request.
    pub backend: PtyBackend,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("failed to open pty: {0}")]
    OpenPty(#[source] anyhow::Error),
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("child i/o error: {0}")]
    Io(#[source] std::io::Error),
    #[error("child already exited")]
    AlreadyExited,
}

/// Sent from the reader task up to the pane provider each time the grid
/// mutates. v0.1 just says "grid changed"; a later revision carries a diff.
#[derive(Debug, Clone, Copy)]
pub enum SessionOutput {
    Redraw,
    Exited { status: u32 },
}

/// Static size struct implementing `alacritty_terminal::grid::Dimensions`.
/// alacritty's `Term::new` takes anything that reports `columns` +
/// `screen_lines`; we ignore scrollback (already configured via `Config`).
#[derive(Copy, Clone, Debug)]
struct TermDims {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        // "total" = visible + scrollback. alacritty scrollback is stored
        // in Grid separately; for the initial dimensions bootstrap we
        // only need visible lines. See `Term::resize` for post-boot
        // scrollback management.
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// No-op event listener. rimeterm doesn't consume alacritty's
/// `MouseCursorDirty` / `Title` / `Bell` / `ChildExit` events at the
/// Session level — the read loop already fires `SessionOutput::Redraw`
/// after every byte batch, which is what our render loop needs.
///
/// Reusing alacritty's own `VoidListener` would be cleaner, but the
/// `T` in `Term<T>` needs to be `Clone + Send + 'static + EventListener`
/// for us to hold it inside an `Arc<Mutex<Term<T>>>`; `VoidListener` is
/// a bare unit struct and satisfies all of those already. Actually we
/// use it verbatim.
type Listener = alacritty_terminal::event::VoidListener;

/// A running PTY session. Cheap to clone — internal handles are `Arc`.
///
/// C17 note: `Term<VoidListener>` lives behind a `parking_lot::Mutex`.
/// alacritty's own event loop uses `FairMutex`, but our access pattern
/// is read-heavy from the render thread and write-heavy from the single
/// read task — the base mutex is fine.
#[derive(Clone)]
#[allow(dead_code)] // events_tx clones are kept for future subscribers
pub struct Session {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    term: Arc<Mutex<Term<Listener>>>,
    /// Writer end for stdin. `None` after the child exits.
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    /// Notification channel for `SessionOutput::*`.
    events_tx: mpsc::UnboundedSender<SessionOutput>,
}

impl Session {
    /// Spawn a child under a fresh PTY. Returns the session plus a receiver
    /// that streams [`SessionOutput`] to the pane provider.
    #[allow(clippy::needless_pass_by_value)]
    pub fn spawn(
        cfg: SessionConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<SessionOutput>), SessionError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                cols: cfg.cols,
                rows: cfg.rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(SessionError::OpenPty)?;

        let mut builder = CommandBuilder::new(cfg.program.clone());
        for arg in &cfg.args {
            builder.arg(arg);
        }
        if let Some(cwd) = &cfg.cwd {
            builder.cwd(cwd);
        }
        for (k, v) in &cfg.env {
            builder.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|source| SessionError::Spawn {
                program: cfg.program.display().to_string(),
                source,
            })?;

        // Slave end is now owned by the child; drop it locally to release resources.
        drop(pair.slave);

        let writer = pair.master.take_writer().map_err(|e| SessionError::Spawn {
            program: cfg.program.display().to_string(),
            source: e,
        })?;

        // Build the alacritty terminal. Scrollback = 5000 lines (matches
        // the old vt100 setup); no cursor-blinking events, no OSC52.
        let term_config = TermConfig {
            scrolling_history: 5000,
            ..TermConfig::default()
        };
        let dims = TermDims {
            columns: cfg.cols as usize,
            screen_lines: cfg.rows as usize,
        };
        let term = Arc::new(Mutex::new(Term::new(
            term_config,
            &dims,
            alacritty_terminal::event::VoidListener,
        )));
        let child = Arc::new(Mutex::new(child));
        let master = Arc::new(Mutex::new(pair.master));

        let (events_tx, events_rx) = mpsc::unbounded_channel();

        // Spawn blocking reader that pumps bytes into the alacritty parser.
        let reader = master
            .lock()
            .try_clone_reader()
            .map_err(|e| SessionError::Spawn {
                program: cfg.program.display().to_string(),
                source: e,
            })?;
        let term_reader = Arc::clone(&term);
        let events_tx_reader = events_tx.clone();
        // Wrap the writer once here; `read_loop` needs a handle to it so
        // it can respond to CSI DA / DSR queries from the child (Ink / TUI
        // apps refuse to draw until those responses arrive).
        let writer_shared: Arc<Mutex<Option<Box<dyn Write + Send>>>> =
            Arc::new(Mutex::new(Some(writer)));
        let writer_for_reader = Arc::clone(&writer_shared);
        tokio::task::spawn_blocking(move || {
            read_loop(reader, term_reader, events_tx_reader, writer_for_reader)
        });

        // Reap the child in a background task so we don't zombie it.
        let child_reaper = Arc::clone(&child);
        let events_tx_reaper = events_tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = child_reaper.lock();
            match guard.wait() {
                Ok(status) => {
                    let raw: u32 = status.exit_code();
                    info!(status = raw, "pty child exited");
                    let _ = events_tx_reaper.send(SessionOutput::Exited { status: raw });
                }
                Err(e) => error!(error = %e, "pty child wait() failed"),
            }
        });

        Ok((
            Self {
                child,
                master,
                term,
                writer: writer_shared,
                events_tx,
            },
            events_rx,
        ))
    }

    /// Send raw bytes to the child. Silent no-op after the child exited.
    pub fn write(&self, bytes: &[u8]) -> Result<(), SessionError> {
        let mut w = self.writer.lock();
        let Some(writer) = w.as_mut() else {
            return Err(SessionError::AlreadyExited);
        };
        writer.write_all(bytes).map_err(SessionError::Io)?;
        writer.flush().map_err(SessionError::Io)
    }

    /// Ask the child to resize. On Windows this ends up in `ResizePseudoConsole`,
    /// which is expensive (~200 μs) — caller should throttle (§19.12.6).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        // Update the alacritty grid first so redraw is instant.
        let dims = TermDims {
            columns: cols as usize,
            screen_lines: rows as usize,
        };
        self.term.lock().resize(dims);
        self.master
            .lock()
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Spawn {
                program: "resize".into(),
                source: e,
            })?;
        Ok(())
    }

    /// Access the alacritty [`Term`] for rendering. Providers should hold
    /// the guard for the shortest time possible; the read loop takes the
    /// same mutex.
    pub fn with_term<R>(&self, f: impl FnOnce(&Term<Listener>) -> R) -> R {
        let t = self.term.lock();
        f(&t)
    }

    /// Snapshot the grid contents as a plain string, optionally trimmed to
    /// the last `rows` visible rows. Walks alacritty's `display_iter`
    /// (which honors the current display offset and hides scrollback
    /// above the viewport) and joins line-by-line.
    pub fn grid_contents(&self, rows: Option<u16>) -> String {
        let full = self.with_term(term_to_string);
        trim_to_last_rows(&full, rows)
    }

    /// Feed bytes directly into the alacritty grid **without** going through
    /// the child's stdin. Used to paint synthetic messages (e.g. "[exit N]"
    /// after the child dies) so the user isn't left staring at an empty
    /// pane wondering why nothing happened.
    ///
    /// This spins up a throwaway `Processor` per call — fine for the
    /// low-frequency "synthetic banner" use case; not for hot paths.
    pub fn inject_grid_bytes(&self, bytes: &[u8]) {
        let mut term = self.term.lock();
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *term, bytes);
    }

    /// Rendered dimensions of the grid — cols, rows. Useful when a caller
    /// wants to know how large a `rows` request can safely be.
    pub fn grid_size(&self) -> (u16, u16) {
        self.with_term(|t| (t.columns() as u16, t.screen_lines() as u16))
    }

    /// Best-effort kill for `Ctrl+Q` shutdown. Ignores errors — reaper will log.
    pub fn kill(&self) {
        let mut guard = self.child.lock();
        let _ = guard.kill();
    }
}

/// Walk `term.grid().display_iter()` and rebuild the visible viewport as
/// a `\n`-separated string. Empty trailing cells inside a row are kept
/// as spaces (matches the old `vt100::Screen::contents()` behavior) so
/// downstream regex / substring matches see the same layout.
///
/// Note: `display_iter` yields cells in row-major order; wide-char
/// spacers are emitted but we skip them so the printable width matches
/// what the user sees.
fn term_to_string<L: EventListener>(term: &Term<L>) -> String {
    let cols = term.columns();
    let mut out = String::with_capacity(term.screen_lines() * (cols + 1));
    let mut current_row: Option<i32> = None;
    let mut row_buf = String::with_capacity(cols);
    for indexed in term.grid().display_iter() {
        let row = indexed.point.line.0;
        if current_row != Some(row) {
            if current_row.is_some() {
                out.push_str(row_buf.trim_end());
                out.push('\n');
                row_buf.clear();
            }
            current_row = Some(row);
        }
        // Skip the paired-half of a wide char — it's already accounted
        // for in the previous cell's `c`.
        if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            || indexed.cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        row_buf.push(indexed.cell.c);
    }
    if !row_buf.is_empty() {
        out.push_str(row_buf.trim_end());
    }
    out
}

/// Trim `full` to the last `rows` lines. `None` returns the input unchanged;
/// `Some(0)` returns an empty string. Kept as a free function so unit tests
/// can exercise the row-slicing logic without spawning a PTY.
pub fn trim_to_last_rows(full: &str, rows: Option<u16>) -> String {
    let Some(n) = rows else {
        return full.to_string();
    };
    if n == 0 {
        return String::new();
    }
    let mut lines: Vec<&str> = full.lines().collect();
    let want = n as usize;
    if lines.len() > want {
        lines = lines.split_off(lines.len() - want);
    }
    lines.join("\n")
}

#[cfg(test)]
mod trim_tests {
    use super::trim_to_last_rows;

    #[test]
    fn none_returns_full_input() {
        assert_eq!(trim_to_last_rows("a\nb\nc", None), "a\nb\nc");
    }

    #[test]
    fn zero_returns_empty() {
        assert_eq!(trim_to_last_rows("a\nb", Some(0)), "");
    }

    #[test]
    fn cap_shorter_than_input_takes_tail() {
        assert_eq!(trim_to_last_rows("a\nb\nc\nd", Some(2)), "c\nd");
    }

    #[test]
    fn cap_larger_than_input_returns_all() {
        assert_eq!(trim_to_last_rows("a\nb", Some(10)), "a\nb");
    }

    #[test]
    fn single_line_input_survives_cap() {
        assert_eq!(trim_to_last_rows("hello", Some(3)), "hello");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(trim_to_last_rows("", Some(5)), "");
        assert_eq!(trim_to_last_rows("", None), "");
    }
}

#[cfg(test)]
mod term_to_string_tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;

    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> Term<VoidListener> {
        let dims = TermDims {
            columns: cols,
            screen_lines: rows,
        };
        let mut t = Term::new(TermConfig::default(), &dims, VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, bytes);
        t
    }

    #[test]
    fn ascii_line_round_trips() {
        let t = term_with(b"hello", 20, 3);
        let s = term_to_string(&t);
        assert!(s.lines().next().unwrap().starts_with("hello"), "got {s:?}");
    }

    #[test]
    fn empty_grid_has_only_blank_lines() {
        let t = term_with(b"", 10, 3);
        let s = term_to_string(&t);
        // All lines are trimmed to empty by trim_end; the join emits
        // (rows-1) newlines with nothing after them.
        assert!(s.chars().all(|c| c == '\n' || c == ' '), "got {s:?}");
    }

    #[test]
    fn wide_char_spacer_is_skipped_not_double_printed() {
        // U+4E2D "中" is a full-width CJK — alacritty writes the char
        // in one cell and marks the next as WIDE_CHAR_SPACER. The
        // spacer must be dropped from the string form, else regex /
        // substring matches would see "中 " with a bogus space.
        let t = term_with("中".as_bytes(), 10, 1);
        let s = term_to_string(&t);
        let first_line = s.lines().next().unwrap_or("");
        assert!(first_line.starts_with('中'), "got {first_line:?}");
        // The next char after 中 should NOT be a stray space from the
        // spacer cell — either end-of-string or trailing rows only.
        let after = first_line.chars().nth(1);
        assert!(after.is_none(), "wide-char spacer leaked: {after:?}");
    }

    #[test]
    fn newlines_separate_rows() {
        // \r\n produces two lines; the second is empty after trim.
        let t = term_with(b"a\r\nb", 10, 3);
        let s = term_to_string(&t);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.first().map(|l| l.trim_end()), Some("a"));
        assert_eq!(lines.get(1).map(|l| l.trim_end()), Some("b"));
    }
}

#[cfg(test)]
mod responder_tests {
    use super::*;
    use parking_lot::Mutex as PlMutex;

    // Sink writes into a parking_lot::Mutex<Vec<u8>> shared across the
    // test — replaces `std::sync::Mutex<Vec<u8>>` because we immediately
    // touch the guard and don't want the poison-recovery boilerplate.
    struct MemWriter {
        inner: Arc<PlMutex<Vec<u8>>>,
    }

    impl Write for MemWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn writer_with_sink() -> (
        Arc<PlMutex<Vec<u8>>>,
        Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    ) {
        let replies = Arc::new(PlMutex::new(Vec::<u8>::new()));
        let writer: Arc<Mutex<Option<Box<dyn Write + Send>>>> =
            Arc::new(Mutex::new(Some(Box::new(MemWriter {
                inner: Arc::clone(&replies),
            }) as Box<dyn Write + Send>)));
        (replies, writer)
    }

    fn fresh_term() -> Arc<Mutex<Term<Listener>>> {
        let dims = TermDims {
            columns: 80,
            screen_lines: 24,
        };
        Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &dims,
            alacritty_terminal::event::VoidListener,
        )))
    }

    #[test]
    fn da1_query_replies_with_vt102_identity() {
        let (replies, writer) = writer_with_sink();
        respond_to_terminal_queries(b"\x1b[c", &fresh_term(), &writer);
        assert_eq!(&*replies.lock(), b"\x1b[?6c");
    }

    #[test]
    fn da2_query_replies_with_unknown_terminal_identity() {
        let (replies, writer) = writer_with_sink();
        respond_to_terminal_queries(b"\x1b[>c", &fresh_term(), &writer);
        assert_eq!(&*replies.lock(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn dsr_status_ok_query_replies_with_zero() {
        let (replies, writer) = writer_with_sink();
        respond_to_terminal_queries(b"\x1b[5n", &fresh_term(), &writer);
        assert_eq!(&*replies.lock(), b"\x1b[0n");
    }

    #[test]
    fn dsr_cursor_position_replies_with_current_pos() {
        let (replies, writer) = writer_with_sink();
        let term = fresh_term();
        // Advance a CUP (row=3, col=5) into the terminal via a fresh
        // Processor. alacritty's cursor is 0-based internally; the
        // responder rebases to 1-based per DSR spec.
        {
            let mut t = term.lock();
            let mut processor: Processor = Processor::new();
            processor.advance(&mut *t, b"\x1b[3;5H");
        }
        respond_to_terminal_queries(b"\x1b[6n", &term, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[3;5R");
    }

    #[test]
    fn non_query_bytes_produce_no_reply() {
        let (replies, writer) = writer_with_sink();
        respond_to_terminal_queries(b"hello world\x1b[31mred\x1b[0m", &fresh_term(), &writer);
        assert!(replies.lock().is_empty());
    }

    #[test]
    fn multiple_queries_in_one_chunk() {
        let (replies, writer) = writer_with_sink();
        respond_to_terminal_queries(b"\x1b[c\x1b[5n", &fresh_term(), &writer);
        assert_eq!(&*replies.lock(), b"\x1b[?6c\x1b[0n");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn read_loop(
    mut reader: Box<dyn std::io::Read + Send>,
    term: Arc<Mutex<Term<Listener>>>,
    tx: mpsc::UnboundedSender<SessionOutput>,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
) {
    // 8 KiB matches ConPTY internal ring size on modern Windows.
    let mut buf = [0u8; 8192];
    // One Processor per session — it holds parser state (partial
    // escape sequences across reads). Recreating it per iteration
    // would drop mid-sequence bytes silently.
    let mut processor: Processor = Processor::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                debug!("pty read loop hit EOF");
                return;
            }
            Ok(n) => {
                let slice = &buf[..n];
                {
                    let mut t = term.lock();
                    processor.advance(&mut *t, slice);
                }
                // Terminal-capability queries: many TUI apps (Ink / React
                // in oh-my-pi, ncurses, prompt-toolkit) block on these
                // before drawing. alacritty's Handler impl doesn't
                // synthesize responses at the terminal-emulator layer,
                // so we do it here from the raw bytes.
                respond_to_terminal_queries(slice, &term, &writer);
                if tx.send(SessionOutput::Redraw).is_err() {
                    // Receiver dropped — session dead.
                    return;
                }
            }
            Err(e) => {
                warn!(error = %e, "pty read loop errored");
                return;
            }
        }
    }
}

/// Scan a chunk of PTY output for common terminal-capability queries and
/// write appropriate responses back into the child's stdin. Only the
/// queries that real-world TUI apps actually block on:
///
/// | query                | reply                                      |
/// |----------------------|--------------------------------------------|
/// | `ESC[c`  / `ESC[0c`  | `ESC[?6c`   — DA1 (VT102)                  |
/// | `ESC[>c` / `ESC[>0c` | `ESC[>0;0;0c` — DA2 (unknown terminal)     |
/// | `ESC[5n`             | `ESC[0n`    — DSR: OK                      |
/// | `ESC[6n`             | `ESC[<row>;<col>R` — cursor position       |
///
/// Any other CSI sequence is left alone; the child either doesn't care or
/// tolerates silence.
fn respond_to_terminal_queries(
    data: &[u8],
    term: &Arc<Mutex<Term<Listener>>>,
    writer: &Arc<Mutex<Option<Box<dyn Write + Send>>>>,
) {
    let mut reply: Vec<u8> = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] != 0x1b || data[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // Skim to the final byte (`@..~`) that ends a CSI sequence.
        let start = i;
        let mut j = i + 2;
        while j < data.len() && !(0x40..=0x7e).contains(&data[j]) {
            j += 1;
        }
        if j >= data.len() {
            break;
        }
        let seq = &data[start..=j];
        match seq {
            b"\x1b[c" | b"\x1b[0c" => reply.extend_from_slice(b"\x1b[?6c"),
            b"\x1b[>c" | b"\x1b[>0c" => reply.extend_from_slice(b"\x1b[>0;0;0c"),
            b"\x1b[5n" => reply.extend_from_slice(b"\x1b[0n"),
            b"\x1b[6n" => {
                // Cursor position: consult the current alacritty grid.
                // alacritty stores cursor as `Point { line: Line(i32),
                // column: Column(usize) }`; DSR wants 1-based row/col.
                let (row, col) = {
                    let t = term.lock();
                    let point: Point = t.grid().cursor.point;
                    // Line can be negative for scrollback rows; clamp
                    // to 0 for the caret query — no shell asks about
                    // scrollback cursor position.
                    let r = point.line.0.max(0) as u32 + 1;
                    let c = point.column.0 as u32 + 1;
                    (r, c)
                };
                use std::io::Write as _;
                let _ = write!(&mut reply, "\x1b[{};{}R", row, col);
            }
            _ => {}
        }
        i = j + 1;
    }
    if reply.is_empty() {
        return;
    }
    let mut w = writer.lock();
    if let Some(w) = w.as_mut() {
        let _ = w.write_all(&reply);
        let _ = w.flush();
    }
}
