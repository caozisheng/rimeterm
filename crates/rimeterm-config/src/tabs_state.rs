//! Per-workspace agent/shell tab state (supersedes the global
//! `ui.state.toml` `agent_tabs` / `shell_tabs` fields for restore).
//!
//! Written to `${data_dir}/workspaces/${workspace_hash}/tabs.state.toml`
//! (duplicates: `tabs-dup${instance}.state.toml`, mirroring the daemon
//! key prefix scheme) so every workspace tab restores ITS OWN pane set:
//!
//! - agents: registry id + stable `slot`. The slot feeds the sessiond
//!   key (`tool-{id}` for slot 0, `tool-{id}-{slot}` otherwise) so two
//!   tabs of the same agent no longer collide on one daemon session and
//!   runtime-opened tabs reattach after restart.
//! - shells: ordinal `number` (drives both the daemon key `shell-{n}`
//!   and the tab title) + last observed cwd, sampled from the child's
//!   root pid at persist time. Restore respawns each shell in its own
//!   remembered directory instead of the workspace root.
//!
//! `next_agent_slot` is a monotonic tombstone: closing a tab never
//! lets a later tab reuse its daemon session key (same pattern as
//! `WorkspacesState::next_instance`).
//!
//! Legacy fallback: when this file is absent the caller falls back to
//! the v0.1 sources — `agents.state.toml` (ids only, slot 0) and the
//! global `ui.state.toml` shell count (cwd = workspace root).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout_state::workspace_hash;
use crate::paths::data_dir;

/// Upper bound on restored shells, matching `MembersPolicy::Open { max: 16 }`.
pub const MAX_SHELL_TABS: usize = 16;

/// One agents-quadrant tab: the registry id plus its daemon-key slot.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTabEntry {
    /// Static registry key (`omp` / `codex` / `claude` / …). Unknown ids
    /// are skipped at restore time.
    pub id: String,
    /// Slot in the daemon session key. `0` = the historical unsuffixed
    /// key, so pre-existing daemon sessions keep reattaching.
    pub slot: u32,
}

/// One shells-quadrant tab: its ordinal and last known working dir.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShellTabEntry {
    /// 1-based ordinal; both the daemon key (`shell-{n}`) and the tab
    /// title derive from it.
    pub number: u32,
    /// Working directory to respawn the shell in when the daemon
    /// session is gone. Falls back to the workspace root when the
    /// directory no longer exists.
    pub cwd: PathBuf,
}

/// Persisted per-workspace tab list.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct TabsState {
    /// Agents-quadrant tabs in display order (first entry = active tab).
    pub agents: Vec<AgentTabEntry>,
    /// Shells-quadrant tabs in display order.
    pub shells: Vec<ShellTabEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum TabsStateError {
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

/// Resolve `${data_dir}/workspaces/${hash}/tabs.state.toml`, or
/// `tabs-dup${instance}.state.toml` for a duplicate workspace instance
/// (instance != 0) so its tab set never shadows the primary's.
pub fn workspace_state_file(workspace_root: &Path, instance: u64) -> Option<PathBuf> {
    let base = data_dir()?;
    let name = if instance == 0 {
        "tabs.state.toml".to_string()
    } else {
        format!("tabs-dup{instance}.state.toml")
    };
    Some(
        base.join("workspaces")
            .join(workspace_hash(workspace_root))
            .join(name),
    )
}

impl TabsState {
    /// Load from `path`. Missing file → default (not an error) so the
    /// caller can fall back to the legacy sources.
    pub fn load_or_default(path: &Path) -> Result<Self, TabsStateError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| TabsStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(TabsStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Persist to `path`, creating the workspace subdirectory if needed.
    pub fn save_to(&self, path: &Path) -> Result<(), TabsStateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| TabsStateError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, body).map_err(|source| TabsStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Legacy migration source: v0.1 `agents.state.toml` ids plus the
    /// global shell count. Slots are 0 (historical unsuffixed daemon
    /// keys reattach), shell cwds fall back to the workspace root.
    pub fn from_legacy(
        agent_ids: &[String],
        shell_count: Option<usize>,
        workspace_root: &Path,
    ) -> Self {
        let agents = agent_ids
            .iter()
            .map(|id| AgentTabEntry {
                id: id.clone(),
                slot: 0,
            })
            .collect();
        let count = shell_count.unwrap_or(1).clamp(1, MAX_SHELL_TABS);
        let shells = (1..=count)
            .map(|n| ShellTabEntry {
                number: n as u32,
                cwd: workspace_root.to_path_buf(),
            })
            .collect();
        Self { agents, shells }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let state = TabsState {
            agents: vec![
                AgentTabEntry {
                    id: "omp".into(),
                    slot: 0,
                },
                AgentTabEntry {
                    id: "omp".into(),
                    slot: 3,
                },
            ],
            shells: vec![ShellTabEntry {
                number: 1,
                cwd: PathBuf::from(r"C:\work\proj"),
            }],
        };
        let text = toml::to_string_pretty(&state).unwrap();
        let back: TabsState = toml::from_str(&text).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile_dir();
        let path = dir.join("tabs.state.toml");
        assert_eq!(
            TabsState::load_or_default(&path).unwrap(),
            TabsState::default()
        );
    }

    #[test]
    fn parse_error_names_the_file() {
        let dir = tempfile_dir();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "agents = 3").unwrap();
        let err = TabsState::load_or_default(&path).unwrap_err();
        assert!(err.to_string().contains("bad.toml"), "got: {err}");
    }

    #[test]
    fn instance_zero_and_duplicates_resolve_distinct_files() {
        let root = Path::new(r"C:\work\proj");
        let primary = workspace_state_file(root, 0).unwrap();
        let dup = workspace_state_file(root, 7).unwrap();
        assert!(primary.to_string_lossy().ends_with("tabs.state.toml"));
        assert!(dup.to_string_lossy().ends_with("tabs-dup7.state.toml"));
        assert_ne!(primary, dup);
    }

    #[test]
    fn legacy_migration_maps_ids_to_slot_zero_and_clamps_count() {
        let ids = vec!["omp".to_string(), "claude".to_string()];
        let state = TabsState::from_legacy(&ids, Some(99), Path::new("/w"));
        assert_eq!(
            state.agents,
            vec![
                AgentTabEntry {
                    id: "omp".into(),
                    slot: 0
                },
                AgentTabEntry {
                    id: "claude".into(),
                    slot: 0
                },
            ]
        );
        assert_eq!(state.shells.len(), MAX_SHELL_TABS);
        assert_eq!(state.shells[0].number, 1);
        assert_eq!(state.shells[0].cwd, PathBuf::from("/w"));
    }

    #[test]
    fn legacy_migration_defaults_to_one_shell() {
        let state = TabsState::from_legacy(&[], None, Path::new("/w"));
        assert_eq!(state.shells.len(), 1);
        assert!(state.agents.is_empty());
    }

    fn tempfile_dir() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }
}
