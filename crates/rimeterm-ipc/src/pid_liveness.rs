//! Probe whether a pid is currently alive.
//!
//! Small platform-conditional helper used by [`crate::server`] to clean up
//! stale lockfiles from crashed or terminated rimeterm processes.
//!
//! - **Unix**: `kill(pid, 0)` returns `Ok(())` when the caller has permission
//!   to signal the process. `ESRCH` = process doesn't exist; anything else we
//!   conservatively treat as alive (permission failure means "someone is
//!   there").
//! - **Windows**: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, pid)` — a
//!   successful open means the process exists (even zombies stay openable
//!   until the last handle drops). We rely on `windows-sys` which we already
//!   pull in transitively via `tokio`.

/// Result of a liveness probe. Kept as an enum (not `bool`) because
/// "we can't tell" is a meaningful third state on Unix: we default to `Alive`
/// so cleanup is conservative — never delete a lockfile that might still be
/// in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidLiveness {
    Alive,
    Dead,
}

impl PidLiveness {
    pub fn is_dead(self) -> bool {
        matches!(self, PidLiveness::Dead)
    }
}

/// Best-effort check whether `pid` is running right now.
pub fn probe(pid: u32) -> PidLiveness {
    if pid == 0 {
        return PidLiveness::Dead;
    }
    if pid == std::process::id() {
        return PidLiveness::Alive;
    }
    probe_impl(pid)
}

#[cfg(unix)]
fn probe_impl(pid: u32) -> PidLiveness {
    // SAFETY: `kill(pid, 0)` is a syscall; passing 0 as the signal is defined
    // to check existence + permission without delivering anything.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return PidLiveness::Alive;
    }
    let err = std::io::Error::last_os_error();
    // ESRCH → no such process; any other errno (e.g. EPERM) means it exists
    // but we can't touch it.
    if err.raw_os_error() == Some(libc::ESRCH) {
        PidLiveness::Dead
    } else {
        PidLiveness::Alive
    }
}

#[cfg(windows)]
fn probe_impl(pid: u32) -> PidLiveness {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: OpenProcess is a well-defined WinAPI call; we always close the
    // handle if we get one. Returns null on failure.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            PidLiveness::Dead
        } else {
            CloseHandle(handle);
            PidLiveness::Alive
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn probe_impl(_pid: u32) -> PidLiveness {
    // Unknown platform: don't dare delete anything.
    PidLiveness::Alive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_pid_is_alive() {
        assert_eq!(probe(std::process::id()), PidLiveness::Alive);
    }

    #[test]
    fn pid_zero_is_dead() {
        assert_eq!(probe(0), PidLiveness::Dead);
    }

    #[test]
    fn very_high_pid_is_dead() {
        // 0xFFFFFFFE is above any plausible pid on either OS. Not guaranteed
        // to be free — but it's the best we can do without spawning.
        assert_eq!(probe(u32::MAX - 1), PidLiveness::Dead);
    }
}
