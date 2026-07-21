//! Environment merge helpers for PTY child processes (C21.5).
//!
//! Every essentials tool + plugin + shell + agent that rimeterm spawns
//! goes through one `default_env(tool_id)` call so the PATH prepend and
//! per-tool config-home overrides live in exactly one place. Two call
//! sites (`shell_factory` and `agent_factory`) both use this helper —
//! adding a new spawn site MUST route through here.
//!
//! Layers (later overrides earlier):
//!
//! 1. **Base**: `PYTHONIOENCODING=utf-8` + `TERM=xterm-256color` — the
//!    §6.2 Windows guarantees.
//! 2. **PATH prepend**: `~/.rimeterm/bin/` + `~/.rimeterm/plugins/*/bin/`
//!    + inherited `$PATH`. See [`crate::paths::augmented_path_env`].
//! 3. **Per-tool config sandbox**: `YAZI_CONFIG_HOME` for yazi,
//!    `XDG_CONFIG_HOME` (Unix) or `APPDATA` (Windows) for gitui,
//!    `BTM_CONFIG_LOCATION` for bottom. Injected only when `tool_id`
//!    matches. Plugins get nothing — their crate-specific env (if any)
//!    is documented in the plugin registry.

/// Build the `env` vec passed to [`rimeterm_pty::SessionConfig`]. Kept
/// static-string-free at the entry point so callers can pass a
/// `&'static str` or an owned `String` interchangeably.
///
/// - `tool_id: None` → generic case (shells, unknown externals). Base
///   env + PATH prepend only.
/// - `tool_id: Some("yazi" | "gitui" | "bottom")` → same as above plus
///   the tool's config-sandbox env.
/// - `tool_id: Some(<plugin>)` → same as `None` for now; plugin
///   registry entries may declare custom env in a future revision.
pub fn default_env(tool_id: Option<&str>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("PYTHONIOENCODING".into(), "utf-8".into()),
        ("TERM".into(), "xterm-256color".into()),
    ];
    if let Some(pair) = crate::paths::augmented_path_env() {
        env.push(pair);
    }
    match tool_id {
        Some("yazi") => {
            if let Some(dir) = crate::paths::yazi_config_dir() {
                env.push(("YAZI_CONFIG_HOME".into(), dir.display().to_string()));
            }
        }
        Some("gitui") => {
            if let Some(home) = crate::paths::home() {
                let key = if cfg!(windows) {
                    "APPDATA"
                } else {
                    "XDG_CONFIG_HOME"
                };
                env.push((key.into(), home.display().to_string()));
            }
        }
        Some("bottom") => {
            if let Some(dir) = crate::paths::bottom_config_dir() {
                let cfg_file = dir.join("bottom.toml");
                env.push(("BTM_CONFIG_LOCATION".into(), cfg_file.display().to_string()));
            }
        }
        _ => {}
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::ENV_LOCK;

    fn with_home<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("RIMETERM_HOME").ok();
        let prev_path = std::env::var("PATH").ok();
        let mut root = std::env::temp_dir();
        let stamp = format!(
            "rimeterm-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        root.push(stamp);
        std::fs::create_dir_all(&root).expect("mkdir env test home");
        unsafe { std::env::set_var("RIMETERM_HOME", &root) };
        unsafe { std::env::set_var("PATH", "/usr/bin") };
        f(&root);
        let _ = std::fs::remove_dir_all(&root);
        match prev {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
        match prev_path {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }

    #[test]
    fn base_env_has_utf8_and_term() {
        with_home(|_| {
            let env = default_env(None);
            assert!(
                env.iter()
                    .any(|(k, v)| k == "PYTHONIOENCODING" && v == "utf-8")
            );
            assert!(
                env.iter()
                    .any(|(k, v)| k == "TERM" && v == "xterm-256color")
            );
            assert!(env.iter().any(|(k, _)| k == "PATH"));
        });
    }

    #[test]
    fn yazi_gets_config_home() {
        with_home(|root| {
            let env = default_env(Some("yazi"));
            let expected = root.join("yazi").display().to_string();
            let hit = env
                .iter()
                .find(|(k, _)| k == "YAZI_CONFIG_HOME")
                .expect("YAZI_CONFIG_HOME injected");
            assert_eq!(hit.1, expected);
        });
    }

    #[test]
    fn gitui_env_key_is_platform_specific() {
        with_home(|root| {
            let env = default_env(Some("gitui"));
            let key = if cfg!(windows) {
                "APPDATA"
            } else {
                "XDG_CONFIG_HOME"
            };
            let hit = env
                .iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("{key} injected"));
            assert_eq!(hit.1, root.display().to_string());
        });
    }

    #[test]
    fn bottom_gets_config_file_path() {
        with_home(|root| {
            let env = default_env(Some("bottom"));
            let expected = root
                .join("bottom")
                .join("bottom.toml")
                .display()
                .to_string();
            let hit = env
                .iter()
                .find(|(k, _)| k == "BTM_CONFIG_LOCATION")
                .expect("BTM_CONFIG_LOCATION injected");
            assert_eq!(hit.1, expected);
        });
    }

    #[test]
    fn unknown_tool_id_has_no_sandbox_env() {
        with_home(|_| {
            let env = default_env(Some("trippy"));
            assert!(env.iter().all(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "YAZI_CONFIG_HOME" | "XDG_CONFIG_HOME" | "APPDATA" | "BTM_CONFIG_LOCATION"
                )
            }));
        });
    }
}
