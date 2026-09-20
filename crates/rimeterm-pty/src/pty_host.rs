//! Native PTY plumbing shared by [`crate::session::Session`] (in-process
//! host) and the session daemon ([`crate::sessiond`]).
//!
//! Both need the same dance: openpty → CommandBuilder (argv + cwd + env)
//! → spawn → take_writer → clone reader. Extracted here so the daemon
//! gets bit-identical spawn semantics (env merging, slave drop order,
//! error mapping) without dragging the alacritty `Term` along — the
//! daemon never parses VT bytes.

use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::session::{SessionConfig, SessionError};

/// Everything worth keeping after a child is alive under a PTY.
///
/// `child` exists only to be moved into the caller's reaper task — after
/// spawn nothing else touches it (kill goes through `killer`, which is
/// immune to the reaper's mutex).
pub(crate) struct NativePty {
    pub(crate) child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    pub(crate) killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    pub(crate) master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    pub(crate) writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    /// Reader end of the master side; one-shot, cloned before spawn.
    pub(crate) reader: Option<Box<dyn std::io::Read + Send>>,
    pub(crate) root_pid: Option<u32>,
}

/// Open a PTY pair and spawn `cfg.program` under it.
pub(crate) fn open_native_pty(cfg: &SessionConfig) -> Result<NativePty, SessionError> {
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

    // Slave end is now owned by the child; drop it locally to release
    // resources.
    drop(pair.slave);

    let writer = pair.master.take_writer().map_err(|e| SessionError::Spawn {
        program: cfg.program.display().to_string(),
        source: e,
    })?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| SessionError::Spawn {
            program: cfg.program.display().to_string(),
            source: e,
        })?;

    let root_pid = child.process_id();
    let killer = child.clone_killer();

    Ok(NativePty {
        child: Arc::new(Mutex::new(child)),
        killer: Arc::new(Mutex::new(killer)),
        master: Arc::new(Mutex::new(pair.master)),
        writer: Arc::new(Mutex::new(Some(writer))),
        reader: Some(reader),
        root_pid,
    })
}

impl NativePty {
    /// Child's OS pid.
    pub(crate) fn root_pid(&self) -> Option<u32> {
        self.root_pid
    }

    /// Write to the child's stdin. `Err(AlreadyExited)` after exit.
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), SessionError> {
        let mut w = self.writer.lock();
        let Some(writer) = w.as_mut() else {
            return Err(SessionError::AlreadyExited);
        };
        writer.write_all(bytes).map_err(SessionError::Io)?;
        writer.flush().map_err(SessionError::Io)
    }

    /// Resize the PTY pair.
    pub(crate) fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
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
            })
    }

    /// Kill the child **and its whole process tree**.
    ///
    /// `ChildKiller::kill` only terminates the root pid. A shell hosting
    /// `vim` (or `cmd /C echo & more`) leaves grandchildren holding the
    /// ConPTY handles, so the master reader never sees EOF and the session
    /// lingers. Killing the tree closes every handle at once.
    pub(crate) fn kill(&self) {
        #[cfg(windows)]
        if let Some(pid) = self.root_pid {
            kill_tree_windows(pid);
        }
        #[cfg(unix)]
        if let Some(pid) = self.root_pid {
            kill_tree_unix(pid);
        }
        // Also nudge the portable-pty killer for good measure (it is a
        // no-op on an already-dead child and idempotent).
        let mut k = self.killer.lock();
        let _ = k.kill();
    }
}

/// Kill a process and all descendants on Windows via `taskkill /T /F`.
///
/// `taskkill` walks the parent-pid chain the OS tracks; `/T` matches the
/// full tree, `/F` forces termination. Fails silently — the fallback
/// `killer.kill()` below still terminates the root.
#[cfg(windows)]
fn kill_tree_windows(pid: u32) {
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {}
        other => {
            tracing::debug!(pid, ?other, "taskkill tree-kill failed; falling back");
        }
    }
}

#[cfg(unix)]
fn kill_tree_unix(pid: u32) {
    // Process-group signal: portable-pty spawns the child in its own
    // process group on Unix (`CommandBuilder` sets the session/pgid), so
    // signaling the group reaches every descendant without /proc walking.
    let pgid = pid as i32;
    let ret = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if ret != 0 {
        tracing::debug!(pid, ret, "killpg failed; falling back");
    }
}
