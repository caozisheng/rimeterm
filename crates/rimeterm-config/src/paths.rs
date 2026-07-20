//! Configuration / cache / data directories.
//!
//! **Layout (v0.2, user interjection):** everything lives under a single
//! top-level dot-dir in `$HOME` — mirroring the way TUI tools like yazi,
//! nushell, starship, and neovim place their user state (a single dir the
//! user can `rm -rf`, back up, or symlink whole).
//!
//! ```text
//! $RIMETERM_HOME (default: $HOME/.rimeterm)
//! ├── config.toml       ← user config
//! ├── cache/            ← unicode probe, future caches
//! └── data/             ← workspaces state, IPC lockfiles
//!     └── run/          ← <pid>.pid lockfiles
//! ```
//!
//! `RIMETERM_HOME` env var overrides the default `~/.rimeterm` root — used
//! by tests and by users who want their config on a separate mount.
//!
//! **Repo-scoped override** — `<repo>/.rimeterm/config.toml` is unchanged
//! and still resolved via [`repo_config_file`].

use std::path::PathBuf;

/// Resolve the top-level `~/.rimeterm` (or `$RIMETERM_HOME`) root.
///
/// Returns `None` only when neither `RIMETERM_HOME` nor `$HOME` can be
/// resolved (rare — headless CI without HOME).
pub fn home() -> Option<PathBuf> {
    if let Ok(env_home) = std::env::var("RIMETERM_HOME")
        && !env_home.is_empty()
    {
        return Some(PathBuf::from(env_home));
    }
    directories::UserDirs::new().map(|u| u.home_dir().join(".rimeterm"))
}

pub fn config_dir() -> Option<PathBuf> {
    home()
}

pub fn config_file() -> Option<PathBuf> {
    home().map(|d| d.join("config.toml"))
}

pub fn cache_dir() -> Option<PathBuf> {
    home().map(|d| d.join("cache"))
}

pub fn data_dir() -> Option<PathBuf> {
    home().map(|d| d.join("data"))
}

/// Resolve the repo-scoped override file: `<repo>/.rimeterm/config.toml`.
///
/// v0.1 does NOT walk upward to find `.git`; caller passes the workspace root.
pub fn repo_config_file(workspace_root: &std::path::Path) -> PathBuf {
    workspace_root.join(".rimeterm").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        // Save & restore env; tests share process state.
        let prev = std::env::var("RIMETERM_HOME").ok();
        // SAFETY: single-threaded test; `unsafe` required by std since 1.85.
        unsafe { std::env::set_var("RIMETERM_HOME", "/tmp/rimeterm-test") };
        assert_eq!(home(), Some(PathBuf::from("/tmp/rimeterm-test")));
        assert_eq!(
            config_file(),
            Some(PathBuf::from("/tmp/rimeterm-test/config.toml"))
        );
        assert_eq!(data_dir(), Some(PathBuf::from("/tmp/rimeterm-test/data")));
        assert_eq!(cache_dir(), Some(PathBuf::from("/tmp/rimeterm-test/cache")));
        match prev {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
    }

    #[test]
    fn empty_env_var_falls_through_to_home() {
        let prev = std::env::var("RIMETERM_HOME").ok();
        unsafe { std::env::set_var("RIMETERM_HOME", "") };
        // Assert home() returns Some (via UserDirs) rather than the empty
        // env — but we can't assume any particular HOME on CI, so just
        // check that it's non-empty when it exists.
        if let Some(p) = home() {
            assert!(
                !p.as_os_str().is_empty(),
                "empty RIMETERM_HOME must fall through, got {p:?}"
            );
        }
        match prev {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
    }

    #[test]
    fn repo_config_file_hangs_off_workspace() {
        let ws = PathBuf::from("/repo/foo");
        assert_eq!(
            repo_config_file(&ws),
            PathBuf::from("/repo/foo/.rimeterm/config.toml")
        );
    }
}
