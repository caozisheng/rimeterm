//! One PTY [`Session`] = one child process + its master fd + its vt100 grid.
//!
//! v0.1 keeps this deliberately small:
//! - blocking `PtyMaster` reader lives on a `spawn_blocking` tokio task
//! - the parsed vt100 grid is protected by a plain `Mutex` — cheap for the
//!   single-pane skeleton; §19.10.2 rewrites the storage story anyway
//! - resize is a synchronous call on the master handle
//!
//! The bigger contracts (tab-group state machine, keep-alive policies, OSC
//! bridge) live in later crates that build on top of this.

use std::io::Write;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
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

/// Sent from the reader task up to the pane provider each time the vt100 grid
/// mutates. v0.1 just says "grid changed"; a later revision carries a diff.
#[derive(Debug, Clone, Copy)]
pub enum SessionOutput {
    Redraw,
    Exited { status: u32 },
}

/// A running PTY session. Cheap to clone — internal handles are `Arc`.
#[derive(Clone)]
#[allow(dead_code)] // events_tx clones are kept for future subscribers
pub struct Session {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    grid: Arc<Mutex<vt100::Parser>>,
    /// Writer end for stdin. `None` after the child exits.
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    /// Notification channel for `SessionOutput::*`.
    events_tx: mpsc::UnboundedSender<SessionOutput>,
}

impl Session {
    /// Spawn a child under a fresh PTY. Returns the session plus a receiver
    /// that streams [`SessionOutput`] to the pane provider.
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
            .map_err(|e| SessionError::OpenPty(anyhow::Error::from(e)))?;

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

        let child = pair.slave.spawn_command(builder).map_err(|source| {
            SessionError::Spawn {
                program: cfg.program.display().to_string(),
                source: anyhow::Error::from(source),
            }
        })?;

        // Slave end is now owned by the child; drop it locally to release resources.
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SessionError::Spawn {
                program: cfg.program.display().to_string(),
                source: anyhow::Error::from(e),
            })?;

        let grid = Arc::new(Mutex::new(vt100::Parser::new(cfg.rows, cfg.cols, 5000)));
        let child = Arc::new(Mutex::new(child));
        let master = Arc::new(Mutex::new(pair.master));

        let (events_tx, events_rx) = mpsc::unbounded_channel();

        // Spawn blocking reader that pumps bytes into the parser.
        let reader = master
            .lock()
            .expect("fresh master mutex")
            .try_clone_reader()
            .map_err(|e| SessionError::Spawn {
                program: cfg.program.display().to_string(),
                source: anyhow::Error::from(e),
            })?;
        let grid_reader = Arc::clone(&grid);
        let events_tx_reader = events_tx.clone();
        // Wrap the writer once here; `read_loop` needs a handle to it so
        // it can respond to CSI DA / DSR queries from the child (Ink / TUI
        // apps refuse to draw until those responses arrive).
        let writer_shared: Arc<Mutex<Option<Box<dyn Write + Send>>>> =
            Arc::new(Mutex::new(Some(writer)));
        let writer_for_reader = Arc::clone(&writer_shared);
        tokio::task::spawn_blocking(move || {
            read_loop(reader, grid_reader, events_tx_reader, writer_for_reader)
        });

        // Reap the child in a background task so we don't zombie it.
        let child_reaper = Arc::clone(&child);
        let events_tx_reaper = events_tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = child_reaper.lock().expect("child mutex");
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
                grid,
                writer: writer_shared,
                events_tx,
            },
            events_rx,
        ))
    }

    /// Send raw bytes to the child. Silent no-op after the child exited.
    pub fn write(&self, bytes: &[u8]) -> Result<(), SessionError> {
        let mut w = self.writer.lock().expect("writer mutex");
        let Some(writer) = w.as_mut() else {
            return Err(SessionError::AlreadyExited);
        };
        writer.write_all(bytes).map_err(SessionError::Io)?;
        writer.flush().map_err(SessionError::Io)
    }

    /// Ask the child to resize. On Windows this ends up in `ResizePseudoConsole`,
    /// which is expensive (~200 μs) — caller should throttle (§19.12.6).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        // Update the vt100 grid first so redraw is instant.
        self.grid
            .lock()
            .expect("grid mutex")
            .screen_mut()
            .set_size(rows, cols);
        self.master
            .lock()
            .expect("master mutex")
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Spawn {
                program: "resize".into(),
                source: anyhow::Error::from(e),
            })?;
        Ok(())
    }

    /// Access the vt100 grid for rendering. Providers should hold the guard
    /// for the shortest time possible; the read loop takes the same mutex.
    pub fn with_grid<R>(&self, f: impl FnOnce(&vt100::Parser) -> R) -> R {
        let g = self.grid.lock().expect("grid mutex");
        f(&g)
    }

    /// Snapshot the grid contents as a plain string, optionally trimmed to
    /// the last `rows` visible rows. See [`trim_to_last_rows`] for the pure
    /// row-slicing logic; this helper just wires it to the live grid.
    pub fn grid_contents(&self, rows: Option<u16>) -> String {
        let full = self.with_grid(|parser| parser.screen().contents());
        trim_to_last_rows(&full, rows)
    }

    /// Feed bytes directly into the vt100 grid **without** going through
    /// the child's stdin. Used to paint synthetic messages (e.g. "[exit N]"
    /// after the child dies) so the user isn't left staring at an empty
    /// pane wondering why nothing happened.
    pub fn inject_grid_bytes(&self, bytes: &[u8]) {
        if let Ok(mut g) = self.grid.lock() {
            g.process(bytes);
        }
    }

    /// Rendered dimensions of the grid — cols, rows. Useful when a caller
    /// wants to know how large a `rows` request can safely be.
    pub fn grid_size(&self) -> (u16, u16) {
        self.with_grid(|parser| {
            let s = parser.screen();
            (s.size().1, s.size().0)
        })
    }

    /// Best-effort kill for `Ctrl+Q` shutdown. Ignores errors — reaper will log.
    pub fn kill(&self) {
        if let Ok(mut guard) = self.child.lock() {
            let _ = guard.kill();
        }
    }
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
mod responder_tests {
    use super::*;
    use parking_lot::Mutex as PlMutex;

    // Sink writes into a parking_lot::Mutex<Vec<u8>> shared across the
    // test — replaces `std::sync::Mutex<Vec<u8>>` because we immediately
    // touch the guard and don't want the poison-recovery boilerplate.
    struct Sink(Arc<PlMutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn writer_with_sink() -> (
        Arc<PlMutex<Vec<u8>>>,
        Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    ) {
        // The outer Mutex must be std::sync to match respond_to_terminal_queries'
        // signature (chosen for the real writer, which lives in Session).
        let replies: Arc<PlMutex<Vec<u8>>> = Arc::new(PlMutex::new(Vec::new()));
        let writer: Arc<Mutex<Option<Box<dyn Write + Send>>>> = Arc::new(Mutex::new(
            Some(Box::new(Sink(Arc::clone(&replies))) as Box<dyn Write + Send>),
        ));
        (replies, writer)
    }

    #[test]
    fn da1_query_produces_reply() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        respond_to_terminal_queries(b"\x1b[c", &grid, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[?6c");
    }

    #[test]
    fn da2_query_produces_reply() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        respond_to_terminal_queries(b"\x1b[>c", &grid, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn dsr_status_reports_ok() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        respond_to_terminal_queries(b"\x1b[5n", &grid, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[0n");
    }

    #[test]
    fn dsr_cursor_position_replies_with_current_pos() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        grid.lock()
            .expect("grid mutex")
            .process(b"\x1b[3;5H");
        respond_to_terminal_queries(b"\x1b[6n", &grid, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[3;5R");
    }

    #[test]
    fn unrelated_bytes_produce_no_reply() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        respond_to_terminal_queries(b"hello world\x1b[31mred\x1b[0m", &grid, &writer);
        assert!(replies.lock().is_empty());
    }

    #[test]
    fn multiple_queries_in_one_chunk() {
        let (replies, writer) = writer_with_sink();
        let grid = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        // DA1 + DSR status back-to-back.
        respond_to_terminal_queries(b"\x1b[c\x1b[5n", &grid, &writer);
        assert_eq!(&*replies.lock(), b"\x1b[?6c\x1b[0n");
    }
}

fn read_loop(
    mut reader: Box<dyn std::io::Read + Send>,
    grid: Arc<Mutex<vt100::Parser>>,
    tx: mpsc::UnboundedSender<SessionOutput>,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
) {
    // 8 KiB matches ConPTY internal ring size on modern Windows.
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                debug!("pty read loop hit EOF");
                return;
            }
            Ok(n) => {
                let slice = &buf[..n];
                if let Ok(mut g) = grid.lock() {
                    g.process(slice);
                }
                // Terminal-capability queries: many TUI apps (Ink / React
                // in oh-my-pi, ncurses, prompt-toolkit) block on these
                // before drawing. `vt100` is display-only and doesn't
                // synthesize responses, so we do it here.
                respond_to_terminal_queries(slice, &grid, &writer);
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
    grid: &Arc<Mutex<vt100::Parser>>,
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
                // Cursor position: consult the current vt100 grid state.
                let (row, col) = if let Ok(g) = grid.lock() {
                    let (r, c) = g.screen().cursor_position();
                    (r + 1, c + 1) // vt100 CSI positions are 1-based
                } else {
                    (1, 1)
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
    if let Ok(mut w) = writer.lock() {
        if let Some(w) = w.as_mut() {
            let _ = w.write_all(&reply);
            let _ = w.flush();
        }
    }
}
