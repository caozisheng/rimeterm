//! Persisted per-workspace visibility + order for the two left-column
//! tab groups.
//!
//! The left column stacks two Fixed tab groups: `files` (top) and `git`
//! (bottom). Each group ships with a mandatory anchor tab that the user
//! can never hide or move (Files for the top group, Git for the bottom
//! one) plus zero or more optional native side-panes.
//!
//! This file stores the user's picks so the next launch reopens the same
//! subset in the same order without a reset. New tab ids that appear
//! after an upgrade fall back to `visible = true` at the end of their
//! group's list — no one loses a feature by staying silent about it.
//!
//! Storage location: `${data_dir}/workspaces/${workspace_hash}/left_tabs.state.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout_state::workspace_hash;
use crate::paths::data_dir;

/// Stable id of the mandatory tab in the left-top group.
pub const ANCHOR_TOP: &str = "files";
/// Stable id of the mandatory tab in the left-bottom group.
pub const ANCHOR_BOTTOM: &str = "git";

fn default_true() -> bool {
    true
}

/// One row in a group's visibility list.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeftTab {
    /// Stable string id (matches the ids in [`crate::left_tabs_state`]
    /// consumer side — e.g. `files`, `todo`, `fr`, `git`, `sysmon`, …).
    pub id: String,
    /// Whether the tab is rendered in the tab strip. Missing in older
    /// files → default `true` (the pre-feature behavior).
    #[serde(default = "default_true")]
    pub visible: bool,
}

impl LeftTab {
    pub fn new(id: impl Into<String>, visible: bool) -> Self {
        Self {
            id: id.into(),
            visible,
        }
    }
}

/// Persisted state for both left-column tab groups.
///
/// Empty vectors mean "no user preference recorded"; on load the
/// consumer fills them with every known tab id in their canonical order
/// (visible). This keeps a fresh install indistinguishable from the
/// default layout even though the file already exists.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LeftTabsState {
    /// Left-top ordering + visibility. First visible entry MUST be
    /// [`ANCHOR_TOP`]; enforced by [`Self::normalize`].
    pub top: Vec<LeftTab>,
    /// Left-bottom ordering + visibility. First visible entry MUST be
    /// [`ANCHOR_BOTTOM`]; enforced by [`Self::normalize`].
    pub bottom: Vec<LeftTab>,
}

impl LeftTabsState {
    /// Reconcile stored state with the current in-code catalog:
    ///
    /// - drop entries whose id is no longer in the catalog (renamed /
    ///   removed pane),
    /// - append any catalog id missing from the file as `visible = true`
    ///   (new-since-upgrade panes stay visible by default),
    /// - force each anchor's `visible = true` and pin it to index 0 of
    ///   its group.
    ///
    /// `top_catalog` / `bottom_catalog` are the master lists of ids the
    /// caller supports right now — canonical order used as tiebreaker
    /// for appended entries.
    pub fn normalize(&mut self, top_catalog: &[&str], bottom_catalog: &[&str]) {
        normalize_group(&mut self.top, top_catalog, ANCHOR_TOP);
        normalize_group(&mut self.bottom, bottom_catalog, ANCHOR_BOTTOM);
    }

    /// True when every group's ordering matches its catalog exactly and
    /// every tab is visible — i.e. the persisted state carries no user
    /// override. Callers use this to delete the file rather than write
    /// a redundant blob.
    pub fn matches_defaults(&self, top_catalog: &[&str], bottom_catalog: &[&str]) -> bool {
        group_is_default(&self.top, top_catalog) && group_is_default(&self.bottom, bottom_catalog)
    }
}

fn normalize_group(list: &mut Vec<LeftTab>, catalog: &[&str], anchor: &str) {
    // Dedup ids (keep first occurrence) and drop unknown ones.
    let mut seen = std::collections::HashSet::new();
    list.retain(|tab| catalog.contains(&tab.id.as_str()) && seen.insert(tab.id.clone()));
    // Append newly-known ids in catalog order.
    for id in catalog {
        if !list.iter().any(|tab| tab.id == *id) {
            list.push(LeftTab::new(*id, true));
        }
    }
    // Hoist the anchor to the front, force visible.
    if let Some(pos) = list.iter().position(|tab| tab.id == anchor) {
        let mut anchor_tab = list.remove(pos);
        anchor_tab.visible = true;
        list.insert(0, anchor_tab);
    } else if catalog.contains(&anchor) {
        list.insert(0, LeftTab::new(anchor, true));
    }
}

fn group_is_default(list: &[LeftTab], catalog: &[&str]) -> bool {
    if list.len() != catalog.len() {
        return false;
    }
    list.iter()
        .zip(catalog.iter())
        .all(|(tab, id)| tab.id == *id && tab.visible)
}

#[derive(Debug, thiserror::Error)]
pub enum LeftTabsStateError {
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

/// Resolve `${data_dir}/workspaces/${hash}/left_tabs.state.toml`.
pub fn workspace_state_file(workspace_root: &Path) -> Option<PathBuf> {
    let base = data_dir()?;
    Some(
        base.join("workspaces")
            .join(workspace_hash(workspace_root))
            .join("left_tabs.state.toml"),
    )
}

impl LeftTabsState {
    /// Load from `path`. Missing file → default (not an error).
    pub fn load_or_default(path: &Path) -> Result<Self, LeftTabsStateError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| LeftTabsStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LeftTabsStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Persist to `path`. When the state matches the code defaults the
    /// file is deleted instead of written — a missing file and an
    /// all-defaults tree are indistinguishable at load time.
    pub fn save_to(
        &self,
        path: &Path,
        top_catalog: &[&str],
        bottom_catalog: &[&str],
    ) -> Result<(), LeftTabsStateError> {
        if self.matches_defaults(top_catalog, bottom_catalog) {
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(LeftTabsStateError::Io {
                    path: path.display().to_string(),
                    source,
                }),
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| LeftTabsStateError::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            let body = toml::to_string_pretty(self)?;
            std::fs::write(path, body).map_err(|source| LeftTabsStateError::Io {
                path: path.display().to_string(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: &[&str] = &["files", "todo", "glab", "fr"];
    const BOTTOM: &[&str] = &["git", "sysmon", "agtop", "models", "stock"];

    #[test]
    fn normalize_fills_defaults_when_empty() {
        let mut state = LeftTabsState::default();
        state.normalize(TOP, BOTTOM);
        assert_eq!(
            state.top,
            TOP.iter()
                .map(|id| LeftTab::new(*id, true))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            state.bottom,
            BOTTOM
                .iter()
                .map(|id| LeftTab::new(*id, true))
                .collect::<Vec<_>>()
        );
        assert!(state.matches_defaults(TOP, BOTTOM));
    }

    #[test]
    fn normalize_hoists_anchor_to_front() {
        let mut state = LeftTabsState {
            top: vec![
                LeftTab::new("todo", true),
                LeftTab::new("files", false), // hidden anchor → must go visible + front.
                LeftTab::new("fr", true),
            ],
            bottom: vec![],
        };
        state.normalize(TOP, BOTTOM);
        assert_eq!(state.top[0].id, "files");
        assert!(state.top[0].visible);
        assert_eq!(state.top[1].id, "todo");
        assert_eq!(state.top[2].id, "fr");
    }

    #[test]
    fn normalize_appends_newly_known_ids_as_visible() {
        let mut state = LeftTabsState {
            top: vec![LeftTab::new("files", true), LeftTab::new("todo", false)],
            bottom: vec![LeftTab::new("git", true), LeftTab::new("agtop", false)],
        };
        state.normalize(TOP, BOTTOM);
        // `fr` is new-since-upgrade for the top group.
        assert_eq!(state.top.last().unwrap().id, "fr");
        assert!(state.top.last().unwrap().visible);
        // Bottom keeps user ordering and gets the remaining ids appended
        // in catalog order.
        let bottom_ids: Vec<&str> = state.bottom.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            bottom_ids,
            vec!["git", "agtop", "sysmon", "models", "stock"]
        );
    }

    #[test]
    fn normalize_drops_unknown_ids() {
        let mut state = LeftTabsState {
            top: vec![
                LeftTab::new("files", true),
                LeftTab::new("mystery", true), // no longer in catalog.
                LeftTab::new("todo", true),
            ],
            bottom: vec![],
        };
        state.normalize(TOP, BOTTOM);
        assert!(state.top.iter().all(|t| t.id != "mystery"));
        assert!(state.top.iter().any(|t| t.id == "todo"));
    }

    #[test]
    fn round_trip_toml_preserves_state() {
        let state = LeftTabsState {
            top: vec![
                LeftTab::new("files", true),
                LeftTab::new("fr", true),
                LeftTab::new("todo", false),
            ],
            bottom: vec![
                LeftTab::new("git", true),
                LeftTab::new("stock", true),
                LeftTab::new("models", false),
            ],
        };
        let body = toml::to_string_pretty(&state).unwrap();
        let parsed: LeftTabsState = toml::from_str(&body).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn matches_defaults_is_true_only_for_canonical_all_visible() {
        let mut state = LeftTabsState::default();
        state.normalize(TOP, BOTTOM);
        assert!(state.matches_defaults(TOP, BOTTOM));
        state.top[1].visible = false;
        assert!(!state.matches_defaults(TOP, BOTTOM));
    }
}
