//! Persisted per-workspace state for the native two-pane file manager.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{layout_state::workspace_hash, paths::data_dir};

/// Persisted active explorer column.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileSideState {
    /// Left explorer column.
    #[default]
    Left,
    /// Right explorer column.
    Right,
}

/// Durable file-manager preferences scoped to one workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesState {
    /// Active explorer column.
    pub active: FileSideState,
    /// Last directory shown by the left column.
    pub left_dir: PathBuf,
    /// Last directory shown by the right column.
    pub right_dir: PathBuf,
    /// Whether hidden entries are visible.
    pub show_hidden: bool,
    /// Stable sort-mode label.
    pub sort: String,
    /// User preference for two visible columns.
    pub dual_pane: bool,
}

impl Default for FilesState {
    fn default() -> Self {
        Self {
            active: FileSideState::Left,
            left_dir: PathBuf::from("."),
            right_dir: PathBuf::from("."),
            show_hidden: false,
            sort: "name".to_owned(),
            dual_pane: true,
        }
    }
}

/// Errors loading or saving [`FilesState`].
#[derive(Debug, thiserror::Error)]
pub enum FilesStateError {
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

impl FilesState {
    /// Replace missing or non-directory paths with `workspace_root`.
    pub fn resolve_for_workspace(mut self, workspace_root: &Path) -> Self {
        if !self.left_dir.is_dir() {
            self.left_dir = workspace_root.to_path_buf();
        }
        if !self.right_dir.is_dir() {
            self.right_dir = workspace_root.to_path_buf();
        }
        self
    }

    /// Load state, returning defaults when the file does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, FilesStateError> {
        match std::fs::read_to_string(path) {
            Ok(source) => toml::from_str(&source).map_err(|source| FilesStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(FilesStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Atomically replace `path` with this state.
    pub fn save_to(&self, path: &Path) -> Result<(), FilesStateError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| FilesStateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| FilesStateError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| FilesStateError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        std::fs::rename(&temp_path, path).map_err(|source| FilesStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

/// Resolve `${data_dir}/workspaces/<hash>/files.state.toml`.
pub fn workspace_state_file(workspace_root: &Path) -> Option<PathBuf> {
    Some(
        data_dir()?
            .join("workspaces")
            .join(workspace_hash(workspace_root))
            .join("files.state.toml"),
    )
}
