//! Configuration / cache / data directories.
//!
//! **Layout (C21.5):** everything lives under a single top-level dot-dir
//! in `$HOME` — mirroring the way TUI tools like yazi, nushell,
//! starship, and neovim place their user state.
//!
//! ```text
//! $RIMETERM_HOME (default: $HOME/.rimeterm)
//! ├── config.toml       ← user config
//! ├── bin/              ← essentials binaries (yazi/gitui/btm/ya)
//! ├── yazi/             ← rimeterm-owned Yazi config sandbox
//! ├── gitui/            ← rimeterm-owned gitui config sandbox
//! ├── bottom/           ← rimeterm-owned bottom config sandbox
//! ├── plugins/          ← extension slot for non-essential tools
//! │   └── <name>/{bin,config}/
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

/// `~/.rimeterm/bin/` — essentials binaries live here (C21.5).
///
/// Populated at first launch by [`crate::essentials::materialize`] from
/// the release archive's sibling `essentials/` folder. `spawn_external`
/// prepends this dir to every child process's `PATH`.
pub fn bin_dir() -> Option<PathBuf> {
    home().map(|d| d.join("bin"))
}

/// `~/.rimeterm/plugins/` — root of the extension slot (C21.5).
///
/// Each non-essential tool the user installs via `tools.install <name>`
/// gets a subdirectory `plugins/<name>/{bin,config}/`; the bin dir is
/// prepended to child `PATH` via [`plugin_bin_dirs`].
pub fn plugins_dir() -> Option<PathBuf> {
    home().map(|d| d.join("plugins"))
}

/// `~/.rimeterm/plugins/<name>/bin/` — one plugin's binary dir.
pub fn plugin_bin_dir(name: &str) -> Option<PathBuf> {
    plugins_dir().map(|d| d.join(name).join("bin"))
}

/// `~/.rimeterm/plugins/<name>/config/` — one plugin's config sandbox.
pub fn plugin_config_dir(name: &str) -> Option<PathBuf> {
    plugins_dir().map(|d| d.join(name).join("config"))
}

/// Enumerate every `plugins/<name>/bin/` under [`plugins_dir`].
///
/// Returns an empty vec when the plugins dir doesn't exist yet (fresh
/// install, no plugins installed). Used by [`augmented_path_env`] to
/// prepend all plugin bin dirs to `PATH`.
pub fn plugin_bin_dirs() -> Vec<PathBuf> {
    let Some(root) = plugins_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let bin = entry.path().join("bin");
        if bin.is_dir() {
            out.push(bin);
        }
    }
    // Sort for deterministic PATH order (matters for tests and for
    // shadowing predictability if two plugins ship the same binary
    // name).
    out.sort();
    out
}

/// `~/.rimeterm/yazi/` — rimeterm-owned Yazi config sandbox (C21.5).
///
/// `spawn_external` for yazi injects `YAZI_CONFIG_HOME=<this dir>`. Users
/// may edit `init.lua` / `yazi.toml` freely; the `plugins/` subdir under
/// this path is rewritten by rimeterm on version bumps.
pub fn yazi_config_dir() -> Option<PathBuf> {
    home().map(|d| d.join("yazi"))
}

/// `~/.rimeterm/gitui/` — rimeterm-owned gitui config sandbox (C21.5).
///
/// Gitui reads `$XDG_CONFIG_HOME/gitui/` (Unix) or `%APPDATA%\gitui\`
/// (Windows). `spawn_external` sets `XDG_CONFIG_HOME=<home>` on Unix or
/// `APPDATA=<home>` on Windows so both resolve to this directory.
pub fn gitui_config_dir() -> Option<PathBuf> {
    home().map(|d| d.join("gitui"))
}

/// `~/.rimeterm/bottom/` — rimeterm-owned bottom config sandbox (C21.5).
///
/// `spawn_external` for bottom injects
/// `BTM_CONFIG_LOCATION=<this dir>/bottom.toml`.
pub fn bottom_config_dir() -> Option<PathBuf> {
    home().map(|d| d.join("bottom"))
}

/// Build a `("PATH", "<bin_dir>:<plugin bins…>:<existing PATH>")` env
/// pair for prepending to child processes (C21.5).
///
/// Returns `None` when neither `bin_dir` nor any plugin bin dir resolves
/// — the caller falls back to inheriting `PATH` unchanged.
pub fn augmented_path_env() -> Option<(String, String)> {
    let mut prefixes: Vec<PathBuf> = Vec::new();
    if let Some(bin) = bin_dir() {
        prefixes.push(bin);
    }
    prefixes.extend(plugin_bin_dirs());
    if prefixes.is_empty() {
        return None;
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let existing = std::env::var("PATH").unwrap_or_default();
    let mut joined: Vec<String> = prefixes.iter().map(|p| p.display().to_string()).collect();
    if !existing.is_empty() {
        joined.push(existing);
    }
    Some(("PATH".into(), joined.join(sep)))
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

    use crate::test_util::ENV_LOCK;

    #[test]
    fn env_override_wins() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

    /// Helper: set `RIMETERM_HOME` to a fresh manual tempdir, run `f`,
    /// then remove the dir and restore env. Env is process-wide so
    /// tests that touch `RIMETERM_HOME` must serialize save/restore
    /// rigorously (rely on `cargo test`'s default sequencing per
    /// thread — no `serial_test` dep added).
    fn with_rimeterm_home<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("RIMETERM_HOME").ok();
        let mut root = std::env::temp_dir();
        let stamp = format!(
            "rimeterm-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        root.push(stamp);
        std::fs::create_dir_all(&root).expect("mkdir test home");
        unsafe { std::env::set_var("RIMETERM_HOME", &root) };
        f(&root);

        let _ = std::fs::remove_dir_all(&root);
        match prev {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
    }

    #[test]
    fn new_dirs_hang_off_home() {
        with_rimeterm_home(|root| {
            assert_eq!(bin_dir(), Some(root.join("bin")));
            assert_eq!(plugins_dir(), Some(root.join("plugins")));
            assert_eq!(
                plugin_bin_dir("trippy"),
                Some(root.join("plugins").join("trippy").join("bin"))
            );
            assert_eq!(
                plugin_config_dir("trippy"),
                Some(root.join("plugins").join("trippy").join("config"))
            );
            assert_eq!(yazi_config_dir(), Some(root.join("yazi")));
            assert_eq!(gitui_config_dir(), Some(root.join("gitui")));
            assert_eq!(bottom_config_dir(), Some(root.join("bottom")));
        });
    }

    #[test]
    fn plugin_bin_dirs_enumerates_installed_plugins() {
        with_rimeterm_home(|root| {
            // Empty plugins dir → empty vec (does not error).
            assert!(plugin_bin_dirs().is_empty());

            // Create plugins/trippy/bin/ and plugins/zellij/bin/.
            let plugins = root.join("plugins");
            std::fs::create_dir_all(plugins.join("trippy").join("bin")).expect("mkdir trippy/bin");
            std::fs::create_dir_all(plugins.join("zellij").join("bin")).expect("mkdir zellij/bin");
            // A plugin dir with no `bin/` (broken install) must be
            // skipped, not returned.
            std::fs::create_dir_all(plugins.join("halfinstalled")).expect("mkdir halfinstalled");

            let got = plugin_bin_dirs();
            assert_eq!(
                got,
                vec![
                    plugins.join("trippy").join("bin"),
                    plugins.join("zellij").join("bin"),
                ],
                "must return only <name>/bin/ dirs that exist, sorted"
            );
        });
    }

    #[test]
    fn augmented_path_env_prepends_managed_dirs() {
        with_rimeterm_home(|root| {
            let prev_path = std::env::var("PATH").ok();
            // Set a well-known PATH we can compare against.
            unsafe { std::env::set_var("PATH", "/usr/bin:/bin") };

            // Fresh home: no plugins yet, bin_dir absent from disk but
            // still a resolvable *path*. Helper must yield `PATH` with
            // bin_dir prefix + existing PATH tail.
            let (k, v) = augmented_path_env().expect("bin_dir resolves");
            assert_eq!(k, "PATH");
            let sep = if cfg!(windows) { ";" } else { ":" };
            let bin = root.join("bin").display().to_string();
            assert!(v.starts_with(&bin), "must start with bin_dir, got {v}");
            assert!(
                v.ends_with(&format!("{sep}/usr/bin:/bin"))
                    || v.ends_with(&format!("{sep}/usr/bin{sep}/bin")),
                "must preserve existing PATH tail, got {v}"
            );

            // Now add a plugin bin dir; helper must include it between
            // bin_dir and existing PATH.
            let plug = root.join("plugins").join("trippy").join("bin");
            std::fs::create_dir_all(&plug).expect("mkdir plug");
            let (_, v2) = augmented_path_env().expect("still resolves");
            let plug_s = plug.display().to_string();
            let bin_idx = v2.find(&bin).expect("bin present");
            let plug_idx = v2.find(&plug_s).expect("plugin present");
            assert!(bin_idx < plug_idx, "bin must come before plugin");

            // Empty PATH is also OK — helper should not emit trailing separator.
            unsafe { std::env::set_var("PATH", "") };
            let (_, v3) = augmented_path_env().expect("still resolves");
            assert!(
                !v3.ends_with(sep),
                "empty PATH must not leave trailing separator, got {v3}"
            );

            match prev_path {
                Some(v) => unsafe { std::env::set_var("PATH", v) },
                None => unsafe { std::env::remove_var("PATH") },
            }
        });
    }
}
