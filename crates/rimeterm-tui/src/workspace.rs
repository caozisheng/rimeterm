//! Workspace bundles — the per-workspace half of workspace-tab
//! multiplexing (see `docs/plans/2026-09-20-workspace-tab-management.md`).
//!
//! Design: the **active** workspace's fields live directly on `App`
//! (every one of the ~10k lines of method bodies keeps working with
//! plain `self.tree` / `self.panes` field access), while **inactive**
//! workspaces are stashed here as `WorkspaceBundle` values. Switching
//! tabs is a field-by-field `mem::swap` between `App` and the stash —
//! zero-copy, and all PTY sessions in background bundles stay alive
//! (each `Session` owns its own daemon connection / native PTY).
//!
//! `PaneId` is a global monotonic counter, so panes from different
//! bundles never collide in the shared flat maps (`session_writes`,
//! `pane_agent_*`).

use std::collections::HashSet;
use std::path::PathBuf;

use rimeterm_config::Config;
use rimeterm_config::memory_state::{UiState, WorkspaceLayoutMode};
use rimeterm_core::focus::FocusManager;
use rimeterm_core::layout::LayoutTree;

use crate::app::LandscapeTabsState;
use crate::pane_registry::PaneRegistry;
use crate::sessions::SessionHost;
use crate::viewer::ViewerOverlayState;

/// Everything that is scoped to one workspace (one tab). The active
/// bundle's fields are held by `App` itself; this struct is the
/// storage shape for inactive ones and the swap unit between the two.
///
/// Field set mirrors the same-named `App` fields one-to-one — see
/// `App::stash_active_bundle` / `App::restore_bundle` for the pairing.
pub(crate) struct WorkspaceBundle {
    /// Startup root of this workspace (defines its identity/title).
    pub workspace_root: PathBuf,
    pub config: Config,
    /// Session-host routing + deterministic key prefix for this root.
    pub host: SessionHost,
    pub key_prefix: String,
    pub shell_choice: rimeterm_pty::ShellChoice,
    pub shell_short: String,
    pub remembered_ui: UiState,
    pub layout_mode: WorkspaceLayoutMode,
    pub landscape_tabs: LandscapeTabsState,
    pub tree: LayoutTree,
    pub panes: PaneRegistry,
    pub focus: FocusManager,
    pub viewer: ViewerOverlayState,
    pub file_manager_pane_id: Option<rimeterm_core::pane::PaneId>,
    pub git_pane_id: Option<rimeterm_core::pane::PaneId>,
    pub last_file_manager_cwd: Option<PathBuf>,
    pub last_file_selection: Option<crate::viewer::SelectionSnapshot>,
    pub active_root: PathBuf,
    pub pinned_pane_ids: HashSet<rimeterm_core::pane::PaneId>,
    pub default_ratios: Vec<(rimeterm_core::layout::SplitPath, Vec<f32>)>,
    /// Left-column tab catalogs for this workspace (PaneIds differ per
    /// workspace even though the ids/labels are static).
    pub left_top_catalog: Vec<crate::app::LeftTabCatalogEntry>,
    pub left_bottom_catalog: Vec<crate::app::LeftTabCatalogEntry>,
    /// Snapshot of the user-level left-tabs preference this bundle's
    /// tree was built under. Compared against the live global state on
    /// restore; a mismatch re-applies the rebuild to the restored tree.
    pub built_left_tabs: rimeterm_config::left_tabs_state::LeftTabsState,
}

/// Stash slot for logical workspace index `logical` when the active
/// workspace is `active` (the active one occupies no stash slot).
pub(crate) fn stash_slot(logical: usize, active: usize) -> usize {
    if logical < active {
        logical
    } else {
        logical - 1
    }
}

/// Display title for a workspace tab: the root's folder basename.
/// Duplicates disambiguate via the caller-supplied occurrence number
/// (`name`, `name 2`, …).
pub(crate) fn workspace_title(root: &std::path::Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

/// Derive stable display titles in tab order. Equal basenames are
/// numbered by occurrence (`name`, `name 2`, ...).
pub(crate) fn workspace_titles(roots: &[PathBuf]) -> Vec<String> {
    let mut occurrences = std::collections::HashMap::<String, usize>::new();
    roots
        .iter()
        .map(|root| {
            let base = workspace_title(root);
            let occurrence = occurrences.entry(base.clone()).or_default();
            *occurrence += 1;
            if *occurrence == 1 {
                base
            } else {
                format!("{base} {occurrence}")
            }
        })
        .collect()
}

/// Resolve an open request to `(logical_index, should_append)`.
pub(crate) fn open_target(roots: &[PathBuf], root: &PathBuf) -> (usize, bool) {
    roots
        .iter()
        .position(|candidate| candidate == root)
        .map_or((roots.len(), true), |index| (index, false))
}

/// Clamp a persisted active index against the restored root count.
pub(crate) fn clamp_active(active: usize, root_count: usize) -> usize {
    active.min(root_count.saturating_sub(1))
}

/// Map an index from the persisted root list into the compacted list of
/// successfully restored workspaces. Missing/failed entries use `fallback`.
pub(crate) fn restored_active(
    persisted_to_restored: &[Option<usize>],
    requested: usize,
    fallback: usize,
) -> usize {
    persisted_to_restored
        .get(requested)
        .copied()
        .flatten()
        .unwrap_or(fallback)
}

/// Logical insertion index for a duplicate of the active workspace.
/// The new tab is always immediately to the active tab's right.
pub(crate) fn duplicate_target(active: usize, root_count: usize) -> usize {
    active.saturating_add(1).min(root_count)
}
/// Allocate a stable duplicate instance id without ever moving backwards.
pub(crate) fn allocate_instance(next: &mut u64) -> u64 {
    let allocated = (*next).max(1);
    *next = allocated.saturating_add(1);
    allocated
}

/// Session instance id for the first bundle built by `App::new`.
/// Disabled workspace tabs and roots absent from persisted state preserve
/// the old single-workspace daemon key.
pub(crate) fn launch_instance(
    enabled: bool,
    instances: &[u64],
    persisted_index: Option<usize>,
) -> u64 {
    if !enabled {
        return 0;
    }
    persisted_index
        .and_then(|index| instances.get(index).copied())
        .unwrap_or(0)
}

/// Locate a workspace by its persisted root + stable instance id.
pub(crate) fn workspace_index(
    roots: &[PathBuf],
    instances: &[u64],
    root: &std::path::Path,
    instance: u64,
) -> Option<usize> {
    roots
        .iter()
        .zip(instances.iter().copied())
        .position(|(candidate, id)| candidate == root && id == instance)
}

#[cfg(test)]
mod instance_allocation_tests {
    use super::*;

    #[test]
    fn closed_instance_ids_are_never_reused() {
        let mut next = 2;
        assert_eq!(allocate_instance(&mut next), 2);
        assert_eq!(allocate_instance(&mut next), 3);
        assert_eq!(next, 4);
    }

    #[test]
    fn launch_instance_requires_enabled_persisted_match() {
        assert_eq!(launch_instance(false, &[2], Some(0)), 0);
        assert_eq!(launch_instance(true, &[2], Some(0)), 2);
        assert_eq!(launch_instance(true, &[2], None), 0);
    }

    #[test]
    fn async_result_routes_to_exact_duplicate_instance() {
        let root = PathBuf::from("/work/demo");
        let roots = vec![root.clone(), root.clone(), PathBuf::from("/work/other")];
        assert_eq!(workspace_index(&roots, &[0, 4, 0], &root, 4), Some(1));
        assert_eq!(workspace_index(&roots, &[0, 4, 0], &root, 3), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_number_duplicate_basenames_in_tab_order() {
        let roots = vec![
            PathBuf::from("/work/alpha"),
            PathBuf::from("/other/beta"),
            PathBuf::from("/copy/alpha"),
            PathBuf::from("/third/alpha"),
        ];

        assert_eq!(
            workspace_titles(&roots),
            vec!["alpha", "beta", "alpha 2", "alpha 3"]
        );
    }

    #[test]
    fn open_target_activates_existing_root_without_append() {
        let roots = vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")];

        assert_eq!(open_target(&roots, &PathBuf::from("/work/b")), (1, false));
    }

    #[test]
    fn open_target_appends_new_root_at_right() {
        let roots = vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")];

        assert_eq!(open_target(&roots, &PathBuf::from("/work/c")), (2, true));
    }

    #[test]
    fn active_index_clamps_to_last_root() {
        assert_eq!(clamp_active(9, 3), 2);
        assert_eq!(clamp_active(9, 0), 0);
    }

    #[test]
    fn restored_active_maps_across_skipped_roots() {
        let restored = [Some(0), None, Some(1)];
        assert_eq!(restored_active(&restored, 2, 0), 1);
        assert_eq!(restored_active(&restored, 1, 0), 0);
    }

    #[test]
    fn duplicate_opens_immediately_right_of_active() {
        assert_eq!(duplicate_target(0, 3), 1);
        assert_eq!(duplicate_target(1, 3), 2);
        assert_eq!(duplicate_target(2, 3), 3);
    }
}
