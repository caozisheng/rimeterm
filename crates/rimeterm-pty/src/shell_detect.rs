//! Shell auto-detection per §6.2.1 of the design doc.
//!
//! Windows probes PowerShell, cmd, then other commonly installed shells.
//! Unix prefers modern interactive shells before POSIX fallbacks.

use std::path::PathBuf;

use tracing::debug;

#[cfg(windows)]
const COMMON_SHELLS: &[&str] = &[
    "pwsh",
    "powershell",
    "cmd",
    "nu",
    "bash",
    "fish",
    "zsh",
    "xonsh",
    "elvish",
];

#[cfg(unix)]
const COMMON_SHELLS: &[&str] = &[
    "fish", "zsh", "bash", "nu", "xonsh", "elvish", "dash", "ksh", "tcsh", "sh",
];

/// Resolved default shell for the current OS.
#[derive(Clone, Debug, PartialEq, Eq)]
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

    /// UI-friendly label for the settings shell picker. Same as
    /// [`Self::short_name`] for cross-platform names; the Unix variant
    /// derives from the executable stem so `bash` / `fish` / `zsh`
    /// stay distinguishable in the picker rather than collapsing to
    /// the generic "unix-shell".
    pub fn display_label(&self) -> String {
        match self {
            Self::Unix(p) => p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unix-shell")
                .to_string(),
            other => other.short_name().to_string(),
        }
    }
}

/// Detect the default shell honoring the config override list first.
///
/// `hints` is the platform-relevant slice of `[core].shell_win` or `shell_unix`;
/// first entry that resolves via `which` wins.
pub fn detect_default_shell(hints: &[String]) -> ShellChoice {
    for hint in hints {
        if let Some(choice) = resolve_shell(hint) {
            debug!(hint, path = %choice.path().expect("resolved shell has path").display(), "shell resolved from config hint");
            return choice;
        }
    }

    #[cfg(windows)]
    for candidate in [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Program Files (x86)\PowerShell\7\pwsh.exe",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            debug!(path = %path.display(), "pwsh 7 found at default install path");
            return ShellChoice::Pwsh7(path);
        }
    }

    for candidate in COMMON_SHELLS {
        if let Some(choice) = resolve_shell(candidate) {
            return choice;
        }
    }

    ShellChoice::None
}

/// Enumerate every shell candidate discoverable on this host, in a
/// deterministic probe order. Powers the Settings shell picker.
///
/// Unlike [`detect_default_shell`] this does NOT short-circuit on the
/// first hit — every hint is resolved independently, then the built-in
/// platform fallbacks (pwsh default install paths on Windows, `sh` on
/// Unix) are appended. Duplicates (same resolved path) are dropped so
/// a hint like `["pwsh", "pwsh.exe"]` doesn't show up twice.
pub fn detect_all_shells(hints: &[String]) -> Vec<ShellChoice> {
    let mut out = Vec::new();

    for hint in hints
        .iter()
        .map(String::as_str)
        .chain(COMMON_SHELLS.iter().copied())
    {
        if let Some(choice) = resolve_shell(hint) {
            push_unique(&mut out, choice);
        }
    }

    #[cfg(windows)]
    for candidate in [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Program Files (x86)\PowerShell\7\pwsh.exe",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            push_unique(&mut out, ShellChoice::Pwsh7(path));
        }
    }

    out
}

fn resolve_shell(hint: &str) -> Option<ShellChoice> {
    which::which(hint).ok().map(classify_path)
}

pub fn classify_path(path: PathBuf) -> ShellChoice {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match stem.as_str() {
        "pwsh" => ShellChoice::Pwsh7(path),
        "powershell" => ShellChoice::WinPs51(path),
        "cmd" => ShellChoice::Cmd(path),
        _ => ShellChoice::Unix(path),
    }
}

pub fn prefer_saved_shell(saved: Option<&std::path::Path>, fallback: ShellChoice) -> ShellChoice {
    saved
        .filter(|path| path.is_file())
        .map(|path| classify_path(path.to_path_buf()))
        .unwrap_or(fallback)
}

fn push_unique(shells: &mut Vec<ShellChoice>, choice: ShellChoice) {
    let Some(path) = choice.path() else {
        return;
    };
    let duplicate = shells.iter().filter_map(ShellChoice::path).any(|existing| {
        #[cfg(windows)]
        {
            existing
                .to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        }
        #[cfg(not(windows))]
        {
            existing == path
        }
    });
    if !duplicate {
        shells.push(choice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_shells_include_modern_cross_platform_choices() {
        assert!(COMMON_SHELLS.contains(&"nu"));
        assert!(COMMON_SHELLS.contains(&"xonsh"));
        assert!(COMMON_SHELLS.contains(&"elvish"));
    }

    #[test]
    fn classification_uses_resolved_executable_name() {
        assert_eq!(
            classify_path(PathBuf::from(r"C:\Tools\pwsh.exe")),
            ShellChoice::Pwsh7(PathBuf::from(r"C:\Tools\pwsh.exe"))
        );
    }

    #[test]
    fn stale_preference_falls_back_to_detected_shell() {
        let fallback = ShellChoice::Unix(PathBuf::from("fallback"));

        assert_eq!(
            prefer_saved_shell(
                Some(std::path::Path::new("missing-shell")),
                fallback.clone()
            ),
            fallback
        );
    }
    #[test]
    fn empty_hints_do_not_crash() {
        let _ = detect_default_shell(&[]);
    }
}
