//! Persisted per-workspace list of agent tabs.
//!
//! When the user picks an agent (via the picker overlay on `[+]` /
//! `Ctrl+T` / click on the "Pick an agent" placeholder), the choice is
//! written to `${data_dir}/workspaces/${workspace_hash}/agents.state.toml`
//! so the next launch reopens the same tabs without prompting.
//!
//! Deliberately narrow scope for v0.1: we record only the **agent id**
//! (the static registry key: `omp` / `codex` / `claude` / `pi`) per tab.
//! Session content, model overrides, chat history etc. are the agent's
//! own responsibility; rimeterm is just the tab-list gatekeeper.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout_state::workspace_hash;
use crate::paths::data_dir;

/// Persisted agents-quadrant state.
///
/// `tabs` order == on-disk order == the order rimeterm will spawn them
/// at startup (first entry = active tab).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AgentsState {
    /// Agent ids picked in this workspace, in spawn order. Any id no
    /// longer in the registry at load time is skipped silently — the
    /// user hasn't lost a whole session, just an obsolete choice.
    pub tabs: Vec<String>,
}

impl AgentsState {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentsStateError {
    #[error("I/O error reading `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error in `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Resolve `${data_dir}/workspaces/${hash}/agents.state.toml`.
pub fn workspace_state_file(workspace_root: &Path) -> Option<PathBuf> {
    let base = data_dir()?;
    Some(
        base.join("workspaces")
            .join(workspace_hash(workspace_root))
            .join("agents.state.toml"),
    )
}

impl AgentsState {
    /// Load from `path`. Missing file → default (not an error).
    pub fn load_or_default(path: &Path) -> Result<Self, AgentsStateError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| AgentsStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(AgentsStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Persist to `path`, creating the workspace subdirectory if needed.
    pub fn save_to(&self, path: &Path) -> Result<(), AgentsStateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AgentsStateError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, body).map_err(|source| AgentsStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_serializes_and_round_trips() {
        let s = AgentsState::default();
        let body = toml::to_string_pretty(&s).unwrap();
        let back: AgentsState = toml::from_str(&body).unwrap();
        assert_eq!(back, s);
        assert!(back.is_empty());
    }

    #[test]
    fn populated_state_round_trips() {
        let s = AgentsState {
            tabs: vec!["omp".to_string(), "codex".to_string()],
        };
        let body = toml::to_string_pretty(&s).unwrap();
        let back: AgentsState = toml::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn save_and_load_round_trip_via_tempdir() {
        // Use a temp file inside the OS temp dir. Deliberately don't touch
        // the real ~/.rimeterm — the test must be hermetic.
        let dir = std::env::temp_dir().join("rimeterm-agents-state-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("agents.state.toml");
        let s = AgentsState {
            tabs: vec!["claude".to_string(), "pi".to_string()],
        };
        s.save_to(&path).unwrap();
        let back = AgentsState::load_or_default(&path).unwrap();
        assert_eq!(back, s);
        // Cleanup so re-runs stay hermetic.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_default_not_error() {
        let path = std::env::temp_dir().join("rimeterm-agents-state-missing.toml");
        let _ = std::fs::remove_file(&path);
        let s = AgentsState::load_or_default(&path).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn workspace_state_file_lives_under_workspaces_hash_dir() {
        let ws = std::path::PathBuf::from("/tmp/rimeterm-agents-state-fake");
        if let Some(p) = workspace_state_file(&ws) {
            let s = p.to_string_lossy();
            assert!(s.contains("workspaces"), "path was {s}");
            assert!(s.ends_with("agents.state.toml"), "path was {s}");
        }
    }
}
