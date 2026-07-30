//! Cross-workspace session state.
//!
//! Persists the last workspace root the user had open so the next
//! launch (without an explicit CLI arg) can resume in the same
//! directory instead of falling back to the process CWD — which, for
//! installed binaries invoked from Start Menu / Spotlight / Dock,
//! points at the install directory rather than anywhere useful.
//!
//! Storage location: `${data_dir}/session.state.toml`. Deliberately
//! kept OUT of the per-workspace `workspaces/<hash>/` shard because
//! it's an app-wide preference, not per-workspace state.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::data_dir;

/// Global (non-workspace-scoped) session preferences.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SessionState {
    /// Last workspace root the app had active at shutdown. `None` on a
    /// fresh install or after the persisted directory was deleted.
    pub last_workspace: Option<PathBuf>,
}

/// Errors loading or saving [`SessionState`].
#[derive(Debug, thiserror::Error)]
pub enum SessionStateError {
    /// Filesystem operation failed.
    #[error("I/O error for `{path}`: {source}")]
    Io {
        /// Affected path.
        path: String,
        /// Original filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Persisted TOML was invalid.
    #[error("TOML parse error in `{path}`: {source}")]
    Parse {
        /// State file path.
        path: String,
        /// TOML parse error.
        #[source]
        source: toml::de::Error,
    },
    /// State could not be serialized.
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl SessionState {
    /// Load state, returning defaults when the file does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, SessionStateError> {
        match std::fs::read_to_string(path) {
            Ok(source) => toml::from_str(&source).map_err(|source| SessionStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(SessionStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Atomically replace `path` with this state.
    pub fn save_to(&self, path: &Path) -> Result<(), SessionStateError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| SessionStateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| SessionStateError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| SessionStateError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        std::fs::rename(&temp_path, path).map_err(|source| SessionStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

/// Resolve `${data_dir}/session.state.toml`.
///
/// Returns `None` when the data dir can't be resolved (headless CI
/// without HOME + `RIMETERM_HOME`).
pub fn session_state_file() -> Option<PathBuf> {
    Some(data_dir()?.join("session.state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_load_save() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("session.state.toml");

        let loaded = SessionState::load_or_default(&path).expect("load missing");
        assert_eq!(loaded, SessionState::default());

        let state = SessionState {
            last_workspace: Some(PathBuf::from("/tmp/example")),
        };
        state.save_to(&path).expect("save");

        let reloaded = SessionState::load_or_default(&path).expect("reload");
        assert_eq!(reloaded, state);
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");
        let loaded = SessionState::load_or_default(&path).expect("load missing");
        assert!(loaded.last_workspace.is_none());
    }
}
