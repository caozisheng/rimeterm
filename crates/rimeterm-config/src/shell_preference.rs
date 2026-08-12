use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Global executable selected by the Settings shell picker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ShellPreference {
    pub path: Option<PathBuf>,
}

impl ShellPreference {
    /// Load a preference file, returning no selection when it does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, ShellPreferenceError> {
        match std::fs::read_to_string(path) {
            Ok(source) => toml::from_str(&source).map_err(|source| ShellPreferenceError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ShellPreferenceError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Atomically persist the selected executable.
    pub fn save_to(&self, path: &Path) -> Result<(), ShellPreferenceError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| ShellPreferenceError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| ShellPreferenceError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| ShellPreferenceError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        std::fs::rename(&temp_path, path).map_err(|source| ShellPreferenceError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShellPreferenceError {
    #[error("I/O error for `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error for `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("serialization error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn preference_round_trips_selected_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.toml");
        let expected = ShellPreference {
            path: Some(PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe")),
        };

        expected.save_to(&path).unwrap();

        assert_eq!(ShellPreference::load_or_default(&path).unwrap(), expected);
    }

    #[test]
    fn missing_preference_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            ShellPreference::load_or_default(&dir.path().join("missing.toml")).unwrap(),
            ShellPreference::default()
        );
    }

    #[test]
    fn global_preference_path_is_under_data_directory() {
        let home = PathBuf::from("/tmp/rimeterm-home");

        assert_eq!(
            crate::paths::shell_preference_file_in(&home),
            home.join("data").join("shell.toml")
        );
    }
}
