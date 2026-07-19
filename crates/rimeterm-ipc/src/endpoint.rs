//! Endpoint address resolution for the local rimeterm IPC.
//!
//! - **Unix** (Linux / macOS): `${runtime_or_data}/rimeterm/<pid>.sock`.
//!   Uses `XDG_RUNTIME_DIR` when set (matches §11 of the design doc), falls
//!   back to `data_dir/run` so tests don't need a special environment.
//! - **Windows**: `\\.\pipe\rimeterm-<pid>` (a canonical named-pipe name;
//!   there's no filesystem step).
//!
//! Discovery: on Unix a `rimectl` invocation can scan the directory for the
//! newest `.sock`. On Windows the pipe name pattern is fixed and readable via
//! the Windows API; v1 asks the user for a `--pid` flag or defaults to the
//! most recent lockfile the server also writes.

use std::path::PathBuf;

/// Base directory for rimeterm's runtime sockets (Unix only).
///
/// - `$XDG_RUNTIME_DIR/rimeterm/` if the env var is set;
/// - else `<data_dir>/run/`.
///
/// Returns `None` when neither location can be resolved (rare — headless CI
/// without HOME).
#[cfg(unix)]
pub fn runtime_dir() -> Option<PathBuf> {
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(rt).join("rimeterm"));
    }
    rimeterm_config::paths::data_dir().map(|p| p.join("run"))
}

/// Resolve the socket path for `pid` on Unix. Returns `None` on Windows.
#[cfg(unix)]
pub fn socket_for_pid(pid: u32) -> Option<PathBuf> {
    Some(runtime_dir()?.join(format!("{}.sock", pid)))
}

#[cfg(not(unix))]
pub fn socket_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// Named-pipe address on Windows. Returns `None` on Unix.
#[cfg(windows)]
pub fn pipe_name_for_pid(pid: u32) -> Option<String> {
    Some(format!(r"\\.\pipe\rimeterm-{}", pid))
}

#[cfg(not(windows))]
pub fn pipe_name_for_pid(_pid: u32) -> Option<String> {
    None
}

/// Public convenience: resolve **the** endpoint for a given pid as a printable
/// string (socket path on Unix, pipe name on Windows).
pub fn endpoint_display_for_pid(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        pipe_name_for_pid(pid)
    }
    #[cfg(unix)]
    {
        Some(socket_for_pid(pid)?.display().to_string())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// A lockfile the server writes on start, sitting next to the socket (or, on
/// Windows, inside `data_dir/run/`). It lets `rimectl` discover the latest
/// rimeterm process pid without OS-specific probing.
pub fn lockfile_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        runtime_dir()
    }
    #[cfg(not(unix))]
    {
        rimeterm_config::paths::data_dir().map(|p| p.join("run"))
    }
}

pub fn lockfile_for_pid(pid: u32) -> Option<PathBuf> {
    Some(lockfile_dir()?.join(format!("{}.pid", pid)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn windows_pipe_name_shape() {
        let name = pipe_name_for_pid(1234).unwrap();
        assert!(name.starts_with(r"\\.\pipe\rimeterm-"));
        assert!(name.ends_with("1234"));
    }

    #[test]
    #[cfg(unix)]
    fn unix_socket_path_ends_with_pid_sock() {
        let path = socket_for_pid(1234).expect("resolvable in test env");
        assert!(path.to_string_lossy().ends_with("1234.sock"));
    }
}
