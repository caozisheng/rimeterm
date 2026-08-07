//! Glab pane configuration — repository URL and token overrides.
//!
//! Persisted to `~/.rimeterm/data/glab.toml`. When present, the Glab
//! pane uses these values instead of auto-detecting from the workspace's
//! git remote.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlabConfig {
    /// Override auto-detected repository URL.
    /// Example: `https://gitlab.com/owner/project` or `https://github.com/owner/project`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,

    /// Personal access token passed via env var to child `glab`/`gh`
    /// processes (`GITLAB_TOKEN` or `GH_TOKEN`). Stored in plaintext —
    /// securing the config file is the user's responsibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl GlabConfig {
    pub fn is_empty(&self) -> bool {
        self.repository_url.is_none() && self.token.is_none()
    }

    pub fn is_github(&self) -> Option<bool> {
        self.repository_url
            .as_deref()
            .map(|url| url.contains("github.com") || url.contains("github:"))
    }

    pub fn load_or_default(path: &Path) -> Result<Self, GlabConfigError> {
        match std::fs::read_to_string(path) {
            Ok(source) => toml::from_str(&source).map_err(|source| GlabConfigError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(GlabConfigError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), GlabConfigError> {
        if self.is_empty() {
            match std::fs::remove_file(path) {
                Ok(()) | Err(_) => return Ok(()),
            }
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| GlabConfigError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| GlabConfigError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(path);
        }
        std::fs::rename(&temp_path, path).map_err(|source| GlabConfigError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

pub fn glab_config_file() -> Option<std::path::PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("glab.toml"))
}

#[derive(Debug, thiserror::Error)]
pub enum GlabConfigError {
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
    fn default_is_empty() {
        let config = GlabConfig::default();
        assert!(config.is_empty());
        assert_eq!(config.is_github(), None);
    }

    #[test]
    fn github_detection_from_url() {
        let config = GlabConfig {
            repository_url: Some("https://github.com/user/repo".into()),
            token: None,
        };
        assert_eq!(config.is_github(), Some(true));
    }

    #[test]
    fn gitlab_detection_from_url() {
        let config = GlabConfig {
            repository_url: Some("https://gitlab.com/user/repo".into()),
            token: None,
        };
        assert_eq!(config.is_github(), Some(false));
    }

    #[test]
    fn round_trip_save_load() {
        let dir =
            std::env::temp_dir().join(format!("rimeterm-glab-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("glab.toml");

        let config = GlabConfig {
            repository_url: Some("https://gitlab.com/owner/project".into()),
            token: Some("glpat-test-token".into()),
        };
        config.save_to(&path).unwrap();

        let loaded = GlabConfig::load_or_default(&path).unwrap();
        assert_eq!(loaded, config);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_config_removes_file() {
        let dir =
            std::env::temp_dir().join(format!("rimeterm-glab-config-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("glab.toml");

        // Write then clear.
        let config = GlabConfig {
            repository_url: Some("https://gitlab.com/x/y".into()),
            token: None,
        };
        config.save_to(&path).unwrap();
        assert!(path.exists());

        GlabConfig::default().save_to(&path).unwrap();
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_default() {
        let path = std::env::temp_dir().join("rimeterm-glab-does-not-exist.toml");
        let config = GlabConfig::load_or_default(&path).unwrap();
        assert!(config.is_empty());
    }
}
