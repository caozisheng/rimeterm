use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Workspace-tab state, maintained by the TUI and read at startup to
/// restore the workspace strip (`rimeterm workspace: a | b [+]`).
///
/// Written to `~/.rimeterm/data/workspaces.state.toml` (respects
/// `RIMETERM_HOME`) — same dedicated-user-state-file pattern as
/// [`crate::sessiond_state`]. `roots` + `active` are only meaningful
/// when `enabled`; on disable the strip hides and startup ignores the
/// list (one workspace, today's behavior).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkspacesState {
    /// Workspace-tab management enabled (Settings → Daemon first row).
    pub enabled: bool,
    /// Workspace roots, in strip order.
    pub roots: Vec<PathBuf>,
    /// Stable per-tab instance ids parallel to `roots`. `0` is the
    /// primary instance for a root; duplicates use non-zero ids in daemon keys.
    #[serde(default)]
    pub instances: Vec<u64>,
    /// Next duplicate instance id. Monotonic tombstone: closing a tab never
    /// permits its daemon session key to be reused by a later duplicate.
    #[serde(default = "default_next_instance")]
    pub next_instance: u64,
    /// Index of the selected tab, clamped on load.
    pub active: usize,
}

impl Default for WorkspacesState {
    fn default() -> Self {
        Self {
            enabled: true,
            roots: Vec::new(),
            instances: Vec::new(),
            next_instance: 1,
            active: 0,
        }
    }
}

fn default_next_instance() -> u64 {
    1
}

impl WorkspacesState {
    /// Load a state file, returning default when it does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, WorkspacesStateError> {
        let state = match std::fs::read_to_string(path) {
            Ok(source) => {
                toml::from_str::<Self>(&source).map_err(|source| WorkspacesStateError::Parse {
                    path: path.display().to_string(),
                    source,
                })?
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(source) => {
                return Err(WorkspacesStateError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let mut state = state;
        if state.instances.len() != state.roots.len() {
            let mut occurrences = std::collections::HashMap::<PathBuf, u64>::new();
            state.instances = state
                .roots
                .iter()
                .map(|root| {
                    let occurrence = occurrences.entry(root.clone()).or_default();
                    *occurrence += 1;
                    if *occurrence == 1 { 0 } else { *occurrence }
                })
                .collect();
        }
        state.active = state.active.min(state.roots.len().saturating_sub(1));
        let minimum_next = state
            .instances
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        state.next_instance = state.next_instance.max(minimum_next);
        Ok(state)
    }

    /// Atomically persist (temp + rename, Windows-safe).
    pub fn save_to(&self, path: &Path) -> Result<(), WorkspacesStateError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| WorkspacesStateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let temp_path = path.with_extension("tmp");
        let body = toml::to_string_pretty(self)?;
        std::fs::write(&temp_path, body).map_err(|source| WorkspacesStateError::Io {
            path: temp_path.display().to_string(),
            source,
        })?;
        replace_file(&temp_path, path).map_err(|source| WorkspacesStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Update only the feature toggle, preserving remembered roots and active tab.
    pub fn save_enabled_to(path: &Path, enabled: bool) -> Result<(), WorkspacesStateError> {
        let mut state = Self::load_or_default(path)?;
        state.enabled = enabled;
        state.save_to(path)
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    match std::fs::rename(temp_path, path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let backup = path.with_extension(format!("rimeterm-backup-{}", std::process::id()));
            if backup.exists() {
                std::fs::remove_file(&backup)?;
            }
            std::fs::rename(path, &backup)?;
            match std::fs::rename(temp_path, path) {
                Ok(()) => {
                    std::fs::remove_file(backup)?;
                    Ok(())
                }
                Err(rename_error) => {
                    let _ = std::fs::rename(&backup, path);
                    Err(rename_error)
                }
            }
        }
        Err(source) => Err(source),
    }
}

/// `~/.rimeterm/data/workspaces.state.toml` (respects `RIMETERM_HOME`),
/// or `None` when no home directory can be resolved.
pub fn workspaces_state_file() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("workspaces.state.toml"))
}

/// Load the current state, best-effort: missing/corrupt file and
/// unresolvable home all fall back to defaults.
pub fn load_current() -> WorkspacesState {
    workspaces_state_file()
        .and_then(|p| WorkspacesState::load_or_default(&p).ok())
        .unwrap_or_default()
}

/// Persist the state to the standard location, best-effort location
/// resolution (unresolvable home = no-op Ok).
pub fn save_current(state: &WorkspacesState) -> Result<(), WorkspacesStateError> {
    let Some(path) = workspaces_state_file() else {
        return Ok(());
    };
    state.save_to(&path)
}

/// Persist only the feature toggle; roots and active tab remain untouched.
pub fn save_enabled_current(enabled: bool) -> Result<(), WorkspacesStateError> {
    let Some(path) = workspaces_state_file() else {
        return Ok(());
    };
    WorkspacesState::save_enabled_to(&path, enabled)
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspacesStateError {
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

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wstest-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("workspaces.state.toml")
    }

    #[test]
    fn roundtrip_preserves_order_and_active() {
        let path = temp_path("rt");
        let state = WorkspacesState {
            enabled: true,
            roots: vec![r"C:\proj\a".into(), r"C:\proj\b".into()],
            active: 1,
            instances: vec![0, 2],
            next_instance: 3,
        };
        state.save_to(&path).unwrap();
        let loaded = WorkspacesState::load_or_default(&path).unwrap();
        assert_eq!(loaded, state);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_default() {
        let path = temp_path("missing").with_file_name("nope.toml");
        let loaded = WorkspacesState::load_or_default(&path).unwrap();
        assert_eq!(loaded, WorkspacesState::default());
    }

    #[test]
    fn missing_file_defaults_enabled() {
        let path = temp_path("missing-enabled").with_file_name("absent.toml");
        assert!(WorkspacesState::load_or_default(&path).unwrap().enabled);
    }
    #[test]
    fn corrupt_file_is_parse_error() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "not = [toml").unwrap();
        assert!(matches!(
            WorkspacesState::load_or_default(&path),
            Err(WorkspacesStateError::Parse { .. })
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn active_is_clamped_to_roots() {
        let path = temp_path("clamp");
        std::fs::write(&path, "enabled = true\nroots = [\"C:\\\\a\"]\nactive = 5\n").unwrap();
        let loaded = WorkspacesState::load_or_default(&path).unwrap();
        assert_eq!(loaded.active, 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn enabled_only_update_preserves_roots_and_active() {
        let path = temp_path("enabled-only");
        let original = WorkspacesState {
            enabled: true,
            roots: vec![PathBuf::from("C:/a"), PathBuf::from("C:/b")],
            active: 1,
            instances: vec![0, 2],
            next_instance: 3,
        };
        original.save_to(&path).unwrap();

        WorkspacesState::save_enabled_to(&path, false).unwrap();

        let loaded = WorkspacesState::load_or_default(&path).unwrap();
        assert!(!loaded.enabled);
        assert_eq!(loaded.roots, original.roots);
        assert_eq!(loaded.active, 1);
        assert_eq!(loaded.instances, original.instances);
        assert_eq!(loaded.next_instance, original.next_instance);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod instance_tests {
    use super::*;

    #[test]
    fn legacy_state_derives_stable_duplicate_instances() {
        let path = std::env::temp_dir().join(format!(
            "wstest-legacy-instances-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "enabled = true\nroots = [\"C:/a\", \"C:/a\", \"C:/b\"]\nactive = 1\n",
        )
        .unwrap();

        let loaded = WorkspacesState::load_or_default(&path).unwrap();
        assert_eq!(loaded.instances, vec![0, 2, 0]);
        std::fs::remove_file(path).unwrap();
    }
}
