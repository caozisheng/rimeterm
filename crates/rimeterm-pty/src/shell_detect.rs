//! Shell auto-detection per §6.2.1 of the design doc.
//!
//! Order on Windows:
//! 1. `pwsh.exe` on PATH with `$PSVersionTable.PSVersion.Major >= 7`
//! 2. pwsh 7 at default install paths (user forgot to add PATH)
//! 3. Windows PowerShell 5.1 (`powershell.exe`)
//! 4. `cmd.exe`
//!
//! On Unix: fish → bash → sh.
//!
//! v0.1 does NOT probe `$PSVersionTable` (needs a spawn + read + parse — round
//! trip we can't afford at startup). Instead we accept `pwsh.exe` on PATH and
//! trust it is v7+; if it isn't the user can override in config.

use std::path::PathBuf;

use tracing::debug;

/// Resolved default shell for the current OS.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellChoice {
    /// PowerShell 7+ at the given path.
    Pwsh7(PathBuf),
    /// Windows PowerShell 5.1 fallback.
    WinPs51(PathBuf),
    /// `cmd.exe` last-resort fallback.
    Cmd(PathBuf),
    /// Any Unix shell we found (fish/bash/sh/…) at the given path.
    Unix(PathBuf),
    /// Nothing found — caller should surface a clear error.
    None,
}

impl ShellChoice {
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Pwsh7(p) | Self::WinPs51(p) | Self::Cmd(p) | Self::Unix(p) => Some(p.as_path()),
            Self::None => None,
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Pwsh7(_) => "pwsh 7",
            Self::WinPs51(_) => "powershell 5.1",
            Self::Cmd(_) => "cmd",
            Self::Unix(_) => "unix-shell",
            Self::None => "none",
        }
    }
}

/// Detect the default shell honoring the config override list first.
///
/// `hints` is the platform-relevant slice of `[core].shell_win` or `shell_unix`;
/// first entry that resolves via `which` wins.
pub fn detect_default_shell(hints: &[String]) -> ShellChoice {
    // 1. Config hints (verbatim in probe order).
    for hint in hints {
        if let Ok(p) = which::which(hint) {
            debug!(hint, path = %p.display(), "shell resolved from config hint");
            return classify(hint, p);
        }
    }

    // 2. Platform-specific fallbacks: pwsh install paths on Windows, `sh` on Unix.
    #[cfg(windows)]
    for cand in [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Program Files (x86)\PowerShell\7\pwsh.exe",
    ] {
        let p = std::path::PathBuf::from(cand);
        if p.exists() {
            debug!(path = %p.display(), "pwsh 7 found at default install path");
            return ShellChoice::Pwsh7(p);
        }
    }

    #[cfg(windows)]
    if let Ok(p) = which::which("powershell.exe") {
        return ShellChoice::WinPs51(p);
    }
    #[cfg(windows)]
    if let Ok(p) = which::which("cmd.exe") {
        return ShellChoice::Cmd(p);
    }

    #[cfg(unix)]
    if let Ok(p) = which::which("sh") {
        return ShellChoice::Unix(p);
    }

    ShellChoice::None
}

fn classify(hint: &str, resolved: PathBuf) -> ShellChoice {
    let stem = std::path::Path::new(hint)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(hint)
        .to_ascii_lowercase();
    match stem.as_str() {
        "pwsh" => ShellChoice::Pwsh7(resolved),
        "powershell" => ShellChoice::WinPs51(resolved),
        "cmd" => ShellChoice::Cmd(resolved),
        _ => ShellChoice::Unix(resolved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hints_do_not_crash() {
        let _ = detect_default_shell(&[]);
    }
}
