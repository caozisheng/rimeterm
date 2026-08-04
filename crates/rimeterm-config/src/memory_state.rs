//! Global opt-in policy and remembered UI state.
//!
//! Both files are shared by every workspace:
//! `${rimeterm_home}/data/memory.toml` stores which categories are remembered,
//! and `${rimeterm_home}/data/ui.state.toml` stores the latest enabled values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::left_tabs_state::LeftTabsState;
use crate::paths;

fn default_true() -> bool {
    true
}

/// User-controlled categories shown in Settings > Memory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MemoryPolicy {
    #[serde(default = "default_true")]
    pub last_workspace: bool,
    #[serde(default = "default_true")]
    pub pane_sizes: bool,
    #[serde(default = "default_true")]
    pub tab_layout: bool,
    #[serde(default = "default_true")]
    pub active_tabs: bool,
    #[serde(default = "default_true")]
    pub agent_tabs: bool,
    #[serde(default = "default_true")]
    pub shell_tabs: bool,
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_true")]
    pub git: bool,
    #[serde(default = "default_true")]
    pub todo: bool,
    #[serde(default = "default_true")]
    pub fast_resume: bool,
    #[serde(default = "default_true")]
    pub sysmon: bool,
    #[serde(default = "default_true")]
    pub agtop: bool,
    #[serde(default = "default_true")]
    pub models: bool,
    #[serde(default = "default_true")]
    pub stock: bool,
    #[serde(default = "default_true")]
    pub zones: bool,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            last_workspace: true,
            pane_sizes: true,
            tab_layout: true,
            active_tabs: true,
            agent_tabs: true,
            shell_tabs: true,
            files: true,
            git: true,
            todo: true,
            fast_resume: true,
            sysmon: true,
            agtop: true,
            models: true,
            stock: true,
            zones: true,
        }
    }
}

/// Active member in each of the four built-in tab groups.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ActiveTabsState {
    pub files: Option<String>,
    pub git: Option<String>,
    pub agents: usize,
    pub shells: usize,
}

/// Extensible stable settings for one native pane.
///
/// Values use stable lowercase labels owned by the pane. This keeps the config
/// crate independent of TUI-only enum types and lets unknown values fall back
/// without making the whole state file unreadable after an upgrade.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct PaneState {
    pub values: BTreeMap<String, String>,
}

/// Latest remembered state shared by every workspace.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiState {
    pub last_workspace: Option<PathBuf>,
    pub pane_sizes: Option<BTreeMap<String, Vec<f32>>>,
    pub tab_layout: Option<LeftTabsState>,
    pub active_tabs: Option<ActiveTabsState>,
    pub agent_tabs: Option<Vec<String>>,
    pub shell_tabs: Option<usize>,
    pub files: Option<PaneState>,
    pub git: Option<PaneState>,
    pub todo: Option<PaneState>,
    pub fast_resume: Option<PaneState>,
    pub sysmon: Option<PaneState>,
    pub agtop: Option<PaneState>,
    pub models: Option<PaneState>,
    pub stock: Option<PaneState>,
    pub zones: Option<PaneState>,
}

impl UiState {
    /// Drop categories disabled by the current policy before use or save.
    pub fn filtered_by(mut self, policy: &MemoryPolicy) -> Self {
        if !policy.last_workspace {
            self.last_workspace = None;
        }
        if !policy.pane_sizes {
            self.pane_sizes = None;
        }
        if !policy.tab_layout {
            self.tab_layout = None;
        }
        if !policy.active_tabs {
            self.active_tabs = None;
        }
        if !policy.agent_tabs {
            self.agent_tabs = None;
        }
        if !policy.shell_tabs {
            self.shell_tabs = None;
        }
        if !policy.files {
            self.files = None;
        }
        if !policy.git {
            self.git = None;
        }
        if !policy.todo {
            self.todo = None;
        }
        if !policy.fast_resume {
            self.fast_resume = None;
        }
        if !policy.sysmon {
            self.sysmon = None;
        }
        if !policy.agtop {
            self.agtop = None;
        }
        if !policy.models {
            self.models = None;
        }
        if !policy.stock {
            self.stock = None;
        }
        if !policy.zones {
            self.zones = None;
        }
        self
    }

    pub fn load_or_default(path: &Path) -> Result<Self, MemoryStateError> {
        load_or_default(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), MemoryStateError> {
        save_atomic(self, path)
    }
}

impl MemoryPolicy {
    pub fn load_or_default(path: &Path) -> Result<Self, MemoryStateError> {
        load_or_default(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), MemoryStateError> {
        save_atomic(self, path)
    }
}

/// Policy and already-filtered UI state loaded together at startup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryState {
    pub policy: MemoryPolicy,
    pub ui: UiState,
}

impl MemoryState {
    pub fn load_from(rimeterm_home: &Path) -> Result<Self, MemoryStateError> {
        let policy = MemoryPolicy::load_or_default(&memory_policy_file(rimeterm_home))?;
        let ui = UiState::load_or_default(&ui_state_file(rimeterm_home))?.filtered_by(&policy);
        Ok(Self { policy, ui })
    }

    pub fn load() -> Result<Self, MemoryStateError> {
        let home = paths::home().ok_or(MemoryStateError::HomeUnavailable)?;
        Self::load_from(&home)
    }
}

/// Resolve the global memory-policy file under an explicit rimeterm home.
pub fn memory_policy_file(rimeterm_home: &Path) -> PathBuf {
    rimeterm_home.join("data").join("memory.toml")
}

/// Resolve the global remembered-UI file under an explicit rimeterm home.
pub fn ui_state_file(rimeterm_home: &Path) -> PathBuf {
    rimeterm_home.join("data").join("ui.state.toml")
}

pub fn default_memory_policy_file() -> Option<PathBuf> {
    paths::home().map(|home| memory_policy_file(&home))
}

pub fn default_ui_state_file() -> Option<PathBuf> {
    paths::home().map(|home| ui_state_file(&home))
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryStateError {
    #[error("rimeterm home directory is unavailable")]
    HomeUnavailable,
    #[error("I/O error for `{path}`: {source}")]
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

fn load_or_default<T>(path: &Path) -> Result<T, MemoryStateError>
where
    T: Default + for<'de> Deserialize<'de>,
{
    match std::fs::read_to_string(path) {
        Ok(source) => toml::from_str(&source).map_err(|source| MemoryStateError::Parse {
            path: path.display().to_string(),
            source,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(MemoryStateError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn save_atomic<T: Serialize>(value: &T, path: &Path) -> Result<(), MemoryStateError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| MemoryStateError::Io {
        path: parent.display().to_string(),
        source,
    })?;
    let temp_path = path.with_extension("tmp");
    let body = toml::to_string_pretty(value)?;
    std::fs::write(&temp_path, body).map_err(|source| MemoryStateError::Io {
        path: temp_path.display().to_string(),
        source,
    })?;
    replace_file(&temp_path, path).map_err(|source| MemoryStateError::Io {
        path: path.display().to_string(),
        source,
    })
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
                    let _restore = std::fs::rename(&backup, path);
                    Err(rename_error)
                }
            }
        }
        Err(source) => Err(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_file_preserves_atomic_replacement_semantics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("memory.toml");
        let replacement = dir.path().join("memory.tmp");
        std::fs::write(&target, "old").expect("write target");
        std::fs::write(&replacement, "new").expect("write replacement");

        replace_file(&replacement, &target).expect("replace existing target");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(!replacement.exists());
    }
}
