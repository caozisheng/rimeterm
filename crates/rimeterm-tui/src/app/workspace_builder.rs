//! Reusable construction of one workspace bundle.
//!
//! This is a child of `app`, so it can reuse the existing private pane
//! factories without widening their public surface.

use super::*;

fn rollback_new_entries<K, V>(
    map: &mut std::collections::HashMap<K, V>,
    before: &std::collections::HashSet<K>,
    mut cleanup: impl FnMut(V),
) where
    K: Copy + Eq + std::hash::Hash,
{
    let created: Vec<K> = map
        .keys()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    for id in created {
        if let Some(value) = map.remove(&id) {
            cleanup(value);
        }
    }
}

impl App {
    pub(super) fn build_workspace_bundle(
        &self,
        workspace_root: PathBuf,
        config: Config,
        memory: rimeterm_config::memory_state::MemoryState,
        explicit_workspace: bool,
        instance_id: u64,
    ) -> Result<WorkspaceBuild> {
        let before: std::collections::HashSet<PaneId> =
            self.session_writes.lock().keys().copied().collect();
        let kill_on_rollback = !SessionHost::from_config(&config).is_daemon();
        let result = self.build_workspace_bundle_inner(
            workspace_root,
            config,
            memory,
            explicit_workspace,
            instance_id,
        );
        if result.is_err() {
            let mut sessions = self.session_writes.lock();
            rollback_new_entries(&mut sessions, &before, |session| {
                if kill_on_rollback {
                    session.kill();
                }
            });
        }
        result
    }

    fn build_workspace_bundle_inner(
        &self,
        workspace_root: PathBuf,
        config: Config,
        memory: rimeterm_config::memory_state::MemoryState,
        explicit_workspace: bool,
        instance_id: u64,
    ) -> Result<WorkspaceBuild> {
        let shell_choice = pick_shell(&config)?;
        let shell_short = shell_status_label(&shell_choice);
        let resolved_root = resolve_workspace_root(&workspace_root);
        let viewer_markdown_theme = self.viewer_markdown_theme;
        let event_bus = self.event_bus.clone();
        let session_writes = Arc::clone(&self.session_writes);
        let redraw_tx = self.redraw_tx.clone();
        let osc_tx = self.osc_tx.clone();
        let mut panes = PaneRegistry::new();
        let mut pinned_pane_ids: std::collections::HashSet<PaneId> =
            std::collections::HashSet::new();

        // Files group — native explorer, Todo, Glab, then Fast Resume.
        let mut file_manager_pane = FileManagerPane::with_event_bus(
            workspace_root.clone(),
            workspace_root.clone(),
            event_bus.clone(),
        );
        if let Some(state) = memory.ui.files.as_ref() {
            if explicit_workspace {
                file_manager_pane.restore_preferences(state);
            } else {
                file_manager_pane.restore_state(state);
            }
        }
        let file_manager_pane_id = file_manager_pane.id();
        panes.insert(Box::new(file_manager_pane));
        pinned_pane_ids.insert(file_manager_pane_id);
        let mut todo_pane = TodoPane::new(self.todo_action_tx.clone(), viewer_markdown_theme);
        if let Some(state) = memory.ui.todo.as_ref() {
            todo_pane.restore_state(state);
        }
        let todo_pane_id = todo_pane.id();
        panes.insert(Box::new(todo_pane));
        pinned_pane_ids.insert(todo_pane_id);
        let mut glab_pane = GlabPane::new(
            resolved_root.clone(),
            Color::White,
            tokio::runtime::Handle::current(),
        );
        if let Some(state) = memory.ui.glab.as_ref() {
            glab_pane.restore_state(state);
        }
        let glab_pane_id = glab_pane.id();
        panes.insert(Box::new(glab_pane));
        pinned_pane_ids.insert(glab_pane_id);

        let mut fr_pane = FrPane::new(self.fr_action_tx.clone());
        if let Some(state) = memory.ui.fast_resume.as_ref() {
            fr_pane.restore_state(state);
        }
        let fr_pane_id = fr_pane.id();
        panes.insert(Box::new(fr_pane));
        pinned_pane_ids.insert(fr_pane_id);

        // Native Git pane — read-only workspace Git panel backed by
        // `gix`. Follows files-cwd via `SetActiveRoot` in the main loop
        // (see `handle_set_active_root`); F5 issues `workspace.pane.reload`.
        let mut git_pane = crate::git_pane::GitPane::new(resolved_root.clone());
        if let Some(state) = memory.ui.git.as_ref() {
            git_pane.restore_state(state);
        }
        let git_pane_id = git_pane.id();
        panes.insert(Box::new(git_pane));
        let mut startup_agent_pids: Vec<(PaneId, u32)> = Vec::new();
        pinned_pane_ids.insert(git_pane_id);
        let mut git_members = vec![git_pane_id];

        let mut agents_members = Vec::new();
        let mut startup_agent_ids: Vec<(PaneId, &'static str)> = Vec::new();
        // Session-host routing + stable key prefix, defined before the
        // first factory call (agents restore) and reused by every later
        // spawn site. Stored on Self for the runtime methods.
        let host = SessionHost::from_config(&config);
        let mut key_prefix = crate::sessions::key_prefix(&workspace_root);
        if instance_id != 0 {
            key_prefix.push_str(&format!("dup{instance_id}-"));
        }
        for spec in &config.agents.tabs {
            let id = build_agent_pane(
                &host,
                &format!("{key_prefix}tool-{}", spec.id),
                &mut panes,
                &session_writes,
                spec,
                &workspace_root,
                redraw_tx.clone(),
                osc_tx.clone(),
            )?;
            if let Some(pid) = session_writes
                .lock()
                .get(&id)
                .and_then(rimeterm_pty::Session::root_pid)
            {
                startup_agent_pids.push((id, pid));
            }
            if let Some(pane) = panes.get_mut(id) {
                pane.set_right_click_paste(config.mouse.right_click_paste);
                pane.set_scrollback_enabled(true);
            }
            agents_members.push(id);
            if let Some(registry_spec) = rimeterm_pty::agent_registry::find(&spec.id) {
                startup_agent_ids.push((id, registry_spec.id));
            }
        }
        if agents_members.is_empty() {
            for id in memory.ui.agent_tabs.clone().unwrap_or_default() {
                let Some(spec) = rimeterm_pty::agent_registry::find(&id) else {
                    warn!(agent_id = id, "remembered agent id no longer exists");
                    continue;
                };
                let external_spec = rimeterm_config::AgentSpec {
                    id: spec.id.to_string(),
                    label: spec.label.to_string(),
                    command: spec.argv.iter().map(|value| value.to_string()).collect(),
                    install_hint: Some(spec.install_hint.to_string()),
                };
                match build_agent_pane(
                    &host,
                    &format!("{key_prefix}tool-{}", external_spec.id),
                    &mut panes,
                    &session_writes,
                    &external_spec,
                    &workspace_root,
                    redraw_tx.clone(),
                    osc_tx.clone(),
                ) {
                    Ok(pane_id) => {
                        if let Some(pid) = session_writes
                            .lock()
                            .get(&pane_id)
                            .and_then(rimeterm_pty::Session::root_pid)
                        {
                            startup_agent_pids.push((pane_id, pid));
                        }
                        if let Some(pane) = panes.get_mut(pane_id) {
                            pane.set_right_click_paste(config.mouse.right_click_paste);
                            pane.set_scrollback_enabled(true);
                        }
                        agents_members.push(pane_id);
                        startup_agent_ids.push((pane_id, spec.id));
                    }
                    Err(error) => {
                        warn!(agent_id = id, %error, "failed to restore agent tab");
                    }
                }
            }
        }
        if agents_members.is_empty() {
            let hint = format_agent_picker_hint();
            let picker = PlaceholderPane::new(AGENT_PICKER_TITLE, hint, "🤖", Color::LightMagenta);
            let id = picker.id();
            panes.insert(Box::new(picker));
            agents_members.push(id);
        }

        let mut sysmon = crate::sysmon_pane::SysmonPane::new();
        if let Some(state) = memory.ui.sysmon.as_ref() {
            sysmon.restore_state(state);
        }
        let sysmon_id = sysmon.id();
        panes.insert(Box::new(sysmon));
        let mut agtop = crate::agtop_pane::AgtopPane::new(Arc::clone(&self.shared_agent_snapshot));
        git_members.push(sysmon_id);

        if let Some(state) = memory.ui.agtop.as_ref() {
            agtop.restore_state(state);
        }
        let agtop_id = agtop.id();
        panes.insert(Box::new(agtop));
        pinned_pane_ids.insert(agtop_id);
        git_members.push(agtop_id);

        let mut models = crate::models_pane::ModelsPane::new();
        if let Some(state) = memory.ui.models.as_ref() {
            models.restore_state(state);
        }
        let models_id = models.id();
        panes.insert(Box::new(models));
        pinned_pane_ids.insert(models_id);
        git_members.push(models_id);

        let stock_watchlist = rimeterm_config::paths::stock_watchlist_file()
            .unwrap_or_else(|| std::env::temp_dir().join("rimeterm-stock-watchlist.toml"));
        let mut stock = crate::stock_pane::StockPane::new(config.stock.clone(), stock_watchlist);
        if let Some(state) = memory.ui.stock.as_ref() {
            stock.restore_state(state);
        }
        let stock_id = stock.id();
        panes.insert(Box::new(stock));
        pinned_pane_ids.insert(stock_id);
        git_members.push(stock_id);

        let zones_watchlist = rimeterm_config::paths::zones_file()
            .unwrap_or_else(|| std::env::temp_dir().join("rimeterm-zones.toml"));
        let mut zones = crate::zones_pane::ZonesPane::new(config.zones.clone(), zones_watchlist);
        if let Some(state) = memory.ui.zones.as_ref() {
            zones.restore_state(state);
        }
        let zones_id = zones.id();

        let pet_state = rimeterm_config::paths::pet_state_file()
            .unwrap_or_else(|| std::env::temp_dir().join("rimeterm-pet-state.json"));
        let pet_lock = rimeterm_config::paths::pet_lock_file()
            .unwrap_or_else(|| std::env::temp_dir().join("rimeterm-pet.lock"));
        let pet = PetPane::new(pet_state, pet_lock, Arc::clone(&self.main_agent_signal));
        let pet_id = pet.id();
        panes.insert(Box::new(pet));
        pinned_pane_ids.insert(pet_id);
        git_members.push(pet_id);

        let game_best = rimeterm_config::paths::game_best_file()
            .unwrap_or_else(|| std::env::temp_dir().join("rimeterm-pacman-best.json"));
        let game = crate::game_pane::GamePane::new(game_best);
        let game_id = game.id();
        panes.insert(Box::new(game));
        pinned_pane_ids.insert(game_id);
        git_members.push(game_id);
        panes.insert(Box::new(zones));
        pinned_pane_ids.insert(zones_id);
        git_members.push(zones_id);

        let shell_count = memory.ui.shell_tabs.unwrap_or(1).clamp(1, 16);
        let (shell_spawns, restore_error) = restore_requested_shells(shell_count, |number| {
            spawn_shell(
                &host,
                &format!("{key_prefix}shell-{number}"),
                &shell_choice,
                workspace_root.clone(),
                format!("shell-{number}"),
                80,
                24,
                redraw_tx.clone(),
                osc_tx.clone(),
            )
        })?;
        if let Some(error) = restore_error {
            warn!(error = %error, restored = shell_spawns.len(), requested = shell_count,
                "stopped restoring additional shell tabs");
        }
        let mut shells_members = Vec::with_capacity(shell_spawns.len());
        for spawn in shell_spawns {
            let pane_id = spawn.pane.id();
            session_writes
                .lock()
                .insert(pane_id, spawn.pane.session().clone());
            panes.insert(Box::new(spawn.pane));
            if let Some(pane) = panes.get_mut(pane_id) {
                pane.set_right_click_paste(config.mouse.right_click_paste);
                pane.set_scrollback_enabled(true);
            }
            shells_members.push(pane_id);
        }
        let left_top_catalog: Vec<LeftTabCatalogEntry> = vec![
            LeftTabCatalogEntry::new(
                rimeterm_config::left_tabs_state::ANCHOR_TOP,
                "Files",
                file_manager_pane_id,
            ),
            LeftTabCatalogEntry::new("todo", "Tuxedo", todo_pane_id),
            LeftTabCatalogEntry::new("fr", "Fast Resume", fr_pane_id),
        ];
        let left_bottom_catalog: Vec<LeftTabCatalogEntry> = vec![
            LeftTabCatalogEntry::new(
                rimeterm_config::left_tabs_state::ANCHOR_BOTTOM,
                "Git",
                git_pane_id,
            ),
            LeftTabCatalogEntry::new("glab", "Glab", glab_pane_id),
            LeftTabCatalogEntry::new("game", "Game", game_id),
            LeftTabCatalogEntry::new("sysmon", "Sysmon", sysmon_id),
            LeftTabCatalogEntry::new("agtop", "Agtop", agtop_id),
            LeftTabCatalogEntry::new("pet", "Pet", pet_id),
            LeftTabCatalogEntry::new("models", "Models", models_id),
            LeftTabCatalogEntry::new("stock", "Stock", stock_id),
            LeftTabCatalogEntry::new("zones", "Zones", zones_id),
        ];
        let top_ids: Vec<&'static str> = left_top_catalog.iter().map(|entry| entry.id).collect();
        let bottom_ids: Vec<&'static str> =
            left_bottom_catalog.iter().map(|entry| entry.id).collect();

        let mut left_tabs_state = memory.ui.tab_layout.clone().unwrap_or_default();
        left_tabs_state.normalize(&top_ids, &bottom_ids);

        let files_members = resolve_left_group_members(&left_top_catalog, &left_tabs_state.top);
        let git_members = resolve_left_group_members(&left_bottom_catalog, &left_tabs_state.bottom);

        let mut files = build_files_group(files_members);
        let mut git = build_git_group(git_members);
        let mut agents = TabGroup::new(
            BUILTIN_AGENTS,
            agents_members,
            MembersPolicy::Open { max: 16 },
            PaneKind::AgentChat,
        );
        let mut shells = TabGroup::new(
            BUILTIN_SHELLS,
            shells_members,
            MembersPolicy::Open { max: 16 },
            PaneKind::Shell,
        );
        if let Some(active) = memory.ui.active_tabs.as_ref() {
            restore_named_active_tab(&mut files, active.files.as_deref(), &left_top_catalog);
            restore_named_active_tab(&mut git, active.git.as_deref(), &left_bottom_catalog);
            let _ = agents.goto(active.agents.min(agents.len().saturating_sub(1)));
            let _ = shells.goto(active.shells.min(shells.len().saturating_sub(1)));
        }

        let landscape_tabs = LandscapeTabsState {
            files: files.members().to_vec(),
            files_active: files.active_index(),
            git: git.members().to_vec(),
            git_active: git.active_index(),
            agents: agents.members().to_vec(),
            agents_active: agents.active_index(),
            shells: shells.members().to_vec(),
            shells_active: shells.active_index(),
            tools_active: files.active_pane(),
        };
        let layout_mode = memory.ui.workspace_layout;
        let mut tree = match layout_mode {
            WorkspaceLayoutMode::Landscape => build_landscape_tree(&landscape_tabs)?,
            WorkspaceLayoutMode::Vertical => build_vertical_tree_from_state(&landscape_tabs)?,
        };
        let default_ratios = snapshot_all_ratios(&tree);

        if let Some(splits) = memory.ui.pane_sizes.clone() {
            apply_persisted_state(
                &mut tree,
                &rimeterm_config::layout_state::LayoutState { splits },
            );
        }

        let mut focus = FocusManager::new(event_bus.clone());
        let (initial_pane, initial_group) = match layout_mode {
            WorkspaceLayoutMode::Landscape => (
                tree.find_tab_group(BUILTIN_FILES)
                    .and_then(TabGroup::active_pane)
                    .unwrap_or(file_manager_pane_id),
                BUILTIN_FILES,
            ),
            WorkspaceLayoutMode::Vertical => (
                tree.find_tab_group(BUILTIN_TOOLS)
                    .and_then(TabGroup::active_pane)
                    .unwrap_or(file_manager_pane_id),
                BUILTIN_TOOLS,
            ),
        };
        focus.set_focus(initial_pane, Some(initial_group));

        let launch_root = workspace_root.clone();
        Ok(WorkspaceBuild {
            bundle: crate::workspace::WorkspaceBundle {
                workspace_root,
                config,
                host,
                key_prefix,
                shell_choice,
                shell_short,
                remembered_ui: memory.ui,
                layout_mode,
                landscape_tabs,
                tree,
                panes,
                focus,
                viewer: ViewerOverlayState::default(),
                file_manager_pane_id: Some(file_manager_pane_id),
                git_pane_id: Some(git_pane_id),
                last_file_manager_cwd: Some(launch_root),
                last_file_selection: None,
                active_root: resolved_root,
                pinned_pane_ids,
                default_ratios,
                left_top_catalog,
                left_bottom_catalog,
                built_left_tabs: left_tabs_state,
            },
            agent_ids: startup_agent_ids,
            agent_pids: startup_agent_pids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::rollback_new_entries;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn failed_build_rolls_back_only_new_entries() {
        let mut map = HashMap::from([(1_u64, "existing"), (2, "new-a"), (3, "new-b")]);
        let before = HashSet::from([1_u64]);
        let mut cleaned = Vec::new();

        rollback_new_entries(&mut map, &before, |value| cleaned.push(value));

        cleaned.sort_unstable();
        assert_eq!(map, HashMap::from([(1_u64, "existing")]));
        assert_eq!(cleaned, vec!["new-a", "new-b"]);
    }
}
