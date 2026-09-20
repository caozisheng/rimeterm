use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// User-level sessiond preferences, maintained from the Settings modal.
/// Written to `~/.rimeterm/sessiond.state.toml` and read at every TUI
/// startup as an **override** over `[core] sessiond` / `[core]
/// sessiond_grace_secs` from the config files: when this file exists and
/// a field is set, it wins over the static config.
///
/// Why a separate file (not rewriting config.toml)? The repo-scope
/// config fully replaces the user config when present, and `Config` has
/// no save path — every other Settings-writable knob (shell preference,
/// memory policy) follows the same dedicated-file pattern.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SessiondState {
    /// Override for `[core] sessiond`. `None` = follow config files.
    pub enabled: Option<bool>,
    /// Override for `[core] sessiond_grace_secs`: how long the daemon
    /// keeps hosting sessions after the last client detaches before it
    /// exits. `None` = follow config files. `Some(0)` = exit as soon as
    /// the last session is gone (previous built-in 5s idle default).
    pub grace_secs: Option<u64>,
}

impl SessiondState {
    /// Load a state file, returning default when it does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, SessiondStateError> {
        match std::fs::read_to_string(path) {
            Ok(source) => toml::from_str(&source).map_err(|source| SessiondStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(SessiondStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Atomically persist.
    pub fn save_to(&self, path: &Path) -> Result<(), SessiondStateError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| SessiondStateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| SessiondStateError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| SessiondStateError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        std::fs::rename(&temp_path, path).map_err(|source| SessiondStateError::Io {
            path: temp_path.display().to_string(),
            source,
        })
    }
}

/// `~/.rimeterm/data/sessiond.state.toml` (respects `RIMETERM_HOME`),
/// or `None` when no home directory can be resolved.
pub fn sessiond_state_file() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("sessiond.state.toml"))
}

/// Load the current override state, best-effort: a missing or corrupt
/// file (and an unresolvable home) all fall back to defaults, matching
/// how the TUI treats every other optional state file.
pub fn load_current() -> SessiondState {
    sessiond_state_file()
        .and_then(|p| SessiondState::load_or_default(&p).ok())
        .unwrap_or_default()
}

/// Persist the override state to the standard location, best-effort.
pub fn save_current(state: &SessiondState) -> Result<(), SessiondStateError> {
    let Some(path) = sessiond_state_file() else {
        return Ok(());
    };
    state.save_to(&path)
}

#[derive(Debug, thiserror::Error)]
pub enum SessiondStateError {
    #[error("I/O error for `{path}`: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("parse error for `{path}`: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("serialization error: {0}")]
    Serialize(#[from] toml::ser::Error),
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = SessiondState::load_or_default(&dir.path().join("absent.toml")).unwrap();
        assert_eq!(loaded, SessiondState::default());
    }
    #[test]
    fn state_round_trips_both_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiond.state.toml");
        let state = SessiondState {
            enabled: Some(true),
            grace_secs: Some(300),
        };
        state.save_to(&path).unwrap();
        let loaded = SessiondState::load_or_default(&path).unwrap();
        assert_eq!(loaded, state);
    }
    #[test]
    fn state_file_path_is_under_rimeterm_home() {
        let Some(path) = sessiond_state_file() else {
            return; // no HOME on this box
        };
        assert!(path.ends_with("sessiond.state.toml"));
    }
}
