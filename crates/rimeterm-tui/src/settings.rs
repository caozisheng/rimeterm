//! Native Settings overlay for the Agents registry, viewer knobs, shell
//! picker, and Explorer-integration toggle.
//!
//! The overlay owns only presentation state. Agent selection is handed
//! back to App so this module never owns pane or command handles.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_pty::ShellChoice;
use rimeterm_pty::agent_registry::DetectedAgent;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Agents,
    /// C22.6: Alt+V viewer knobs. Currently only the markdown theme
    /// picker lives here.
    Viewer,
    /// System shell picker: choose which shell (`pwsh` / `powershell` /
    /// `cmd` / `bash` / `fish` / …) is spawned when the user opens a
    /// new shell tab. Rows are populated by
    /// [`rimeterm_pty::detect_all_shells`].
    Shell,
    /// Left-column tab picker: choose which optional tabs are visible
    /// in the left-top (`files`) and left-bottom (`git`) groups and in
    /// what order. Files and Git anchor each group and are always
    /// visible at position 0.
    Tabs,
    /// OS-shell integration: install / uninstall the "Open with
    /// rimeterm here" right-click entry on Explorer folder + folder
    /// background. Windows-only for now (writes HKCU registry
    /// entries — no admin needed); other platforms render a
    /// "not supported" notice.
    Integration,
    /// Global remembered-state categories shared by every workspace.
    Memory,
    /// Glab pane configuration: repository URL override and
    /// authentication token for GitLab/GitHub CLI backends.
    Glab,
}

impl Default for SettingsTab {
    fn default() -> Self {
        Self::Agents
    }
}

/// Which left-column tab group a mutation is aimed at.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LeftGroup {
    /// Top group in the left column (`files`).
    Top,
    /// Bottom group in the left column (`git`).
    Bottom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsAction {
    Agent {
        id: String,
    },
    /// C22.6: user picked a new markdown viewer theme. App applies it
    /// live (next viewer frame picks it up) + persists it to the
    /// current-workspace state store.
    SetMarkdownTheme(rimeterm_markdown::Theme),
    /// User picked a new system shell. App swaps `self.shell_choice`;
    /// existing shell tabs keep their PTY, next new shell inherits.
    SetShell(ShellChoice),
    /// Register the "Open with rimeterm here" Explorer right-click
    /// entry (Windows: HKCU registry write, no admin).
    InstallContextMenu,
    /// Remove the "Open with rimeterm here" Explorer right-click
    /// entry (Windows: HKCU registry delete).
    UninstallContextMenu,
    /// Left-column tab list has been mutated (visibility toggled or
    /// reordered). Payload is the FULL new state — App swaps its own
    /// copy, rebuilds the matching tab group's members via
    /// `TabGroup::set_members`, and flushes to disk. Whole-state
    /// replace keeps the two sides of the mutation in lock-step, and
    /// avoids re-implementing normalize / anchor rules on the App
    /// side.
    SetLeftTabsState(rimeterm_config::left_tabs_state::LeftTabsState),
    SetMemoryPolicy(rimeterm_config::memory_state::MemoryPolicy),
    ApplyGlabConfig(rimeterm_config::glab_config::GlabConfig),
    ClearGlabConfig,
    Refresh,
    Close,
}

/// Which field is currently being edited in the Glab settings tab.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GlabEditField {
    RepositoryUrl,
    Token,
}

#[derive(Debug)]
pub struct SettingsState {
    pub open: bool,
    pub tab: SettingsTab,
    pub cursor: usize,
    pub agents: Vec<DetectedAgent>,
    /// Populated by [`Self::refresh`] on open. Empty if the host has
    /// no shells at all (extremely unusual).
    pub shells: Vec<ShellChoice>,
    /// Platform-specific hints from the loaded application config.
    pub shell_hints: Vec<String>,
    pub busy: Option<String>,
    /// C22.6: theme currently applied in the markdown viewer, shown as
    /// the highlighted row when the Viewer tab is active. Callers seed
    /// this via `set_markdown_theme` before opening the overlay so the
    /// initial cursor lands on the "current" row.
    pub markdown_theme: rimeterm_markdown::Theme,
    /// Shell currently in use, seeded from `App::shell_choice` right
    /// before opening the overlay. Drives the Shell tab's cursor snap
    /// + the ● marker on the "current" row. Defaults to
    /// [`ShellChoice::None`] which never matches — safe fallback.
    pub current_shell: ShellChoice,
    /// Whether the OS-shell right-click integration is currently
    /// installed. `None` when unknown / not yet probed, `Some(true)`
    /// when the registry entry exists, `Some(false)` when it doesn't
    /// (or the platform isn't supported). Refreshed by
    /// [`Self::refresh`] and after each install / uninstall.
    pub integration_installed: Option<bool>,
    /// Live copy of the persisted left-column tab visibility + order,
    /// seeded from App on open. All mutations happen here (Space
    /// toggle, Shift+Up/Down reorder); each yields a
    /// [`SettingsAction::SetLeftTabsState`] with the fresh state so
    /// App can rebuild the tab groups and flush to disk in one step.
    pub left_tabs_state: rimeterm_config::left_tabs_state::LeftTabsState,
    /// Human-readable labels for every left-column tab id shown in
    /// the Tabs panel. Missing ids render as their raw id string
    /// (harmless fallback, but should not happen — App seeds every
    /// catalog entry).
    pub left_tab_labels: std::collections::HashMap<String, String>,
    /// Live copy of the global remembered-state policy.
    pub memory_policy: rimeterm_config::memory_state::MemoryPolicy,
    /// Glab pane configuration — seeded from App on open.
    pub glab_config: rimeterm_config::glab_config::GlabConfig,
    /// Which Glab field is being inline-edited, if any.
    pub glab_editing: Option<GlabEditField>,
    /// Text buffer for the field currently being edited.
    pub glab_edit_buffer: String,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            open: false,
            tab: SettingsTab::default(),
            cursor: 0,
            agents: Vec::new(),
            shells: Vec::new(),
            shell_hints: Vec::new(),
            busy: None,
            markdown_theme: rimeterm_markdown::Theme::default(),
            current_shell: ShellChoice::None,
            integration_installed: None,
            left_tabs_state: rimeterm_config::left_tabs_state::LeftTabsState::default(),
            left_tab_labels: std::collections::HashMap::new(),
            memory_policy: rimeterm_config::memory_state::MemoryPolicy::default(),
            glab_config: rimeterm_config::glab_config::GlabConfig::default(),
            glab_editing: None,
            glab_edit_buffer: String::new(),
        }
    }
}

impl SettingsState {
    pub fn open(&mut self) {
        self.open = true;
        self.tab = SettingsTab::default();
        self.cursor = 0;
        self.refresh();
    }

    /// C22.6: seed the "current" theme so the Viewer tab highlights
    /// the active row when the overlay opens. Callers do this from
    /// `App::open_settings_overlay` right after `open()`.
    pub fn set_markdown_theme(&mut self, theme: rimeterm_markdown::Theme) {
        self.markdown_theme = theme;
    }

    /// Seed the "current" shell so the Shell tab highlights the row
    /// matching what's actually spawning new shells. Called from
    /// `App::open_settings_overlay` right after `open()`.
    pub fn set_current_shell(&mut self, shell: ShellChoice) {
        if shell != ShellChoice::None
            && !self
                .shells
                .iter()
                .any(|candidate| candidate.path() == shell.path())
        {
            self.shells.push(shell.clone());
        }
        self.current_shell = shell;
    }

    pub fn set_shell_hints(&mut self, hints: Vec<String>) {
        self.shell_hints = hints;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.busy = None;
    }

    pub fn refresh(&mut self) {
        self.agents = rimeterm_pty::agent_registry::detect_all();
        self.refresh_shells();
        self.integration_installed = crate::shell_integration::probe();
        self.cursor = self.cursor.min(self.row_count().saturating_sub(1));
    }

    fn refresh_shells(&mut self) {
        self.shells = rimeterm_pty::detect_all_shells(&self.shell_hints);
        if let Some(path) = self.current_shell.path()
            && path.is_file()
            && !self.shells.iter().any(|shell| shell.path() == Some(path))
        {
            self.shells.push(self.current_shell.clone());
        }
    }

    /// Reflect the outcome of an install / uninstall action so the
    /// Integration tab's status marker updates without a full refresh.
    /// Called by `App::apply_settings_action`.
    pub fn set_integration_installed(&mut self, installed: Option<bool>) {
        self.integration_installed = installed;
    }

    /// Seed the left-column tab state so the Tabs panel matches the
    /// live workspace when the overlay opens. Called from
    /// `App::open_settings_overlay` right after `open()`. `labels`
    /// pairs stable ids with display strings for both groups; anything
    /// missing from the map falls back to the raw id in the row
    /// renderer.
    pub fn set_left_tabs_state(
        &mut self,
        state: rimeterm_config::left_tabs_state::LeftTabsState,
        labels: std::collections::HashMap<String, String>,
    ) {
        self.left_tabs_state = state;
        self.left_tab_labels = labels;
    }

    pub fn set_memory_policy(&mut self, policy: rimeterm_config::memory_state::MemoryPolicy) {
        self.memory_policy = policy;
    }

    pub fn set_glab_config(&mut self, config: rimeterm_config::glab_config::GlabConfig) {
        self.glab_config = config;
        self.glab_editing = None;
        self.glab_edit_buffer.clear();
    }

    fn row_count(&self) -> usize {
        match self.tab {
            SettingsTab::Agents => self.agents.len(),
            SettingsTab::Viewer => rimeterm_markdown::Theme::ALL.len(),
            SettingsTab::Shell => self.shells.len(),
            SettingsTab::Tabs => self.left_tabs_state.top.len() + self.left_tabs_state.bottom.len(),
            // Integration: two rows on Windows (Install / Uninstall);
            // on other platforms zero rows — the body is a static
            // "not supported" notice.
            SettingsTab::Integration => {
                if cfg!(windows) {
                    2
                } else {
                    0
                }
            }
            SettingsTab::Memory => MEMORY_LABELS.len(),
            SettingsTab::Glab => GLAB_ROW_COUNT,
        }
    }

    /// Translate the flat cursor index used by the Tabs panel into
    /// (group, index-within-group). Returns `None` for cursors that
    /// fall outside both groups (should not happen when
    /// [`Self::row_count`] is honored).
    fn tabs_cursor_target(&self) -> Option<(LeftGroup, usize)> {
        let top_len = self.left_tabs_state.top.len();
        if self.cursor < top_len {
            Some((LeftGroup::Top, self.cursor))
        } else {
            let idx = self.cursor - top_len;
            (idx < self.left_tabs_state.bottom.len()).then_some((LeftGroup::Bottom, idx))
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let count = self.row_count();
        if count == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = ((self.cursor as isize + delta).rem_euclid(count as isize)) as usize;
    }

    /// Toggle visibility of the row under the cursor in the Tabs panel.
    /// Anchor rows silently reject the toggle — the row rendering
    /// carries the `[locked]` badge that explains why. Returns the
    /// [`SettingsAction::SetLeftTabsState`] payload when the mutation
    /// actually changes state.
    fn toggle_left_tab_at_cursor(&mut self) -> Option<SettingsAction> {
        let (group, idx) = self.tabs_cursor_target()?;
        let list = self.left_tabs_state_list_mut(group);
        let entry = list.get_mut(idx)?;
        if is_anchor(group, &entry.id) {
            return None; // Files / Git are mandatory — refuse the flip.
        }
        entry.visible = !entry.visible;
        Some(SettingsAction::SetLeftTabsState(
            self.left_tabs_state.clone(),
        ))
    }

    /// Move the row under the cursor by `delta` positions within its
    /// group. Anchors stay pinned at index 0 (both the anchor row
    /// itself and its neighbor refuse to swap past it). Returns the
    /// payload action when the mutation succeeded, or `None` when the
    /// move would violate the anchor pin or run off either end.
    fn move_left_tab_at_cursor(&mut self, delta: isize) -> Option<SettingsAction> {
        let (group, idx) = self.tabs_cursor_target()?;
        if is_anchor(group, &self.left_tabs_state_list(group)[idx].id) {
            return None; // Anchor never moves.
        }
        let target_idx = (idx as isize).checked_add(delta)?;
        let list = self.left_tabs_state_list_mut(group);
        if target_idx < 1 || target_idx as usize >= list.len() {
            // Position 0 is reserved for the anchor; refuse to swap
            // into it. Off-the-end is a normal boundary case.
            return None;
        }
        let target_idx = target_idx as usize;
        list.swap(idx, target_idx);
        // Follow the moved row so repeated Shift+↑/Shift+↓ keeps
        // pushing the same entry — matches how VS Code, Firefox tab
        // reorder, and every file manager treats keyboard reorders.
        let top_len = self.left_tabs_state.top.len();
        self.cursor = match group {
            LeftGroup::Top => target_idx,
            LeftGroup::Bottom => top_len + target_idx,
        };
        Some(SettingsAction::SetLeftTabsState(
            self.left_tabs_state.clone(),
        ))
    }

    fn left_tabs_state_list(
        &self,
        group: LeftGroup,
    ) -> &[rimeterm_config::left_tabs_state::LeftTab] {
        match group {
            LeftGroup::Top => &self.left_tabs_state.top,
            LeftGroup::Bottom => &self.left_tabs_state.bottom,
        }
    }

    fn left_tabs_state_list_mut(
        &mut self,
        group: LeftGroup,
    ) -> &mut Vec<rimeterm_config::left_tabs_state::LeftTab> {
        match group {
            LeftGroup::Top => &mut self.left_tabs_state.top,
            LeftGroup::Bottom => &mut self.left_tabs_state.bottom,
        }
    }

    fn toggle_memory_at_cursor(&mut self) -> Option<SettingsAction> {
        toggle_memory_policy(&mut self.memory_policy, self.cursor)?;
        Some(SettingsAction::SetMemoryPolicy(self.memory_policy.clone()))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SettingsAction> {
        use crossterm::event::KeyModifiers;
        if !self.open {
            return None;
        }
        // Glab inline editing mode: capture all keys before anything else.
        if self.tab == SettingsTab::Glab && self.glab_editing.is_some() {
            return self.handle_glab_edit_key(key);
        }
        // Tabs-panel-specific mutation keys are checked BEFORE the
        // shared arrow-key cursor movement so `Shift+Up` reorders
        // instead of just moving the cursor.
        if self.tab == SettingsTab::Tabs {
            let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
            match key.code {
                KeyCode::Char(' ') => return self.toggle_left_tab_at_cursor(),
                KeyCode::Char('+') | KeyCode::Char(']') => {
                    return self.move_left_tab_at_cursor(1);
                }
                KeyCode::Char('-') | KeyCode::Char('[') => {
                    return self.move_left_tab_at_cursor(-1);
                }
                KeyCode::Up if shifted => return self.move_left_tab_at_cursor(-1),
                KeyCode::Down if shifted => return self.move_left_tab_at_cursor(1),
                _ => {}
            }
        }
        if self.tab == SettingsTab::Memory && key.code == KeyCode::Char(' ') {
            return self.toggle_memory_at_cursor();
        }
        // Glab tab: 'd' clears the field under cursor.
        if self.tab == SettingsTab::Glab && key.code == KeyCode::Char('d') {
            return self.glab_clear_field_at_cursor();
        }
        match key.code {
            KeyCode::Esc => Some(SettingsAction::Close),
            KeyCode::Tab => {
                // Cycle through every Settings section.
                self.tab = match self.tab {
                    SettingsTab::Agents => SettingsTab::Viewer,
                    SettingsTab::Viewer => SettingsTab::Shell,
                    SettingsTab::Shell => SettingsTab::Tabs,
                    SettingsTab::Tabs => SettingsTab::Integration,
                    SettingsTab::Integration => SettingsTab::Memory,
                    SettingsTab::Memory => SettingsTab::Glab,
                    SettingsTab::Glab => SettingsTab::Agents,
                };
                self.reset_cursor_for_tab();
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.tab = match self.tab {
                    SettingsTab::Agents => SettingsTab::Glab,
                    SettingsTab::Viewer => SettingsTab::Agents,
                    SettingsTab::Shell => SettingsTab::Viewer,
                    SettingsTab::Tabs => SettingsTab::Shell,
                    SettingsTab::Integration => SettingsTab::Tabs,
                    SettingsTab::Memory => SettingsTab::Integration,
                    SettingsTab::Glab => SettingsTab::Memory,
                };
                self.reset_cursor_for_tab();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab = match self.tab {
                    SettingsTab::Agents => SettingsTab::Viewer,
                    SettingsTab::Viewer => SettingsTab::Shell,
                    SettingsTab::Shell => SettingsTab::Tabs,
                    SettingsTab::Tabs => SettingsTab::Integration,
                    SettingsTab::Integration => SettingsTab::Memory,
                    SettingsTab::Memory => SettingsTab::Glab,
                    SettingsTab::Glab => SettingsTab::Agents,
                };
                self.reset_cursor_for_tab();
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => Some(SettingsAction::Refresh),
            KeyCode::Enter => self.selected_action(),
            _ => None,
        }
    }

    fn selected_action(&mut self) -> Option<SettingsAction> {
        match self.tab {
            SettingsTab::Agents => self.agents.get(self.cursor).and_then(|agent| {
                agent.is_available().then(|| SettingsAction::Agent {
                    id: agent.id.to_string(),
                })
            }),
            SettingsTab::Viewer => rimeterm_markdown::Theme::ALL
                .get(self.cursor)
                .copied()
                .map(SettingsAction::SetMarkdownTheme),
            SettingsTab::Shell => self
                .shells
                .get(self.cursor)
                .cloned()
                .map(SettingsAction::SetShell),
            // Enter on the Tabs panel is treated as "toggle" so users
            // who never notice the Space hint still discover the flow.
            // toggle_left_tab_at_cursor takes &mut self, hence the
            // outer method also needs &mut self.
            SettingsTab::Tabs => self.toggle_left_tab_at_cursor(),
            // Row 0 = Install, row 1 = Uninstall on Windows; other
            // platforms have no rows so the match arm is dead.
            SettingsTab::Integration => {
                if !cfg!(windows) {
                    return None;
                }
                match self.cursor {
                    0 => Some(SettingsAction::InstallContextMenu),
                    1 => Some(SettingsAction::UninstallContextMenu),
                    _ => None,
                }
            }
            SettingsTab::Memory => self.toggle_memory_at_cursor(),
            SettingsTab::Glab => self.glab_selected_action(),
        }
    }

    // -- Glab tab helpers ---------------------------------------------------

    fn handle_glab_edit_key(&mut self, key: KeyEvent) -> Option<SettingsAction> {
        use crossterm::event::KeyModifiers;
        match key.code {
            KeyCode::Esc => {
                self.glab_editing = None;
                self.glab_edit_buffer.clear();
                None
            }
            KeyCode::Enter => {
                let value = if self.glab_edit_buffer.is_empty() {
                    None
                } else {
                    Some(self.glab_edit_buffer.clone())
                };
                match self.glab_editing {
                    Some(GlabEditField::RepositoryUrl) => {
                        self.glab_config.repository_url = value;
                    }
                    Some(GlabEditField::Token) => {
                        self.glab_config.token = value;
                    }
                    None => {}
                }
                self.glab_editing = None;
                self.glab_edit_buffer.clear();
                None
            }
            KeyCode::Backspace => {
                self.glab_edit_buffer.pop();
                None
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        let clean = text.lines().next().unwrap_or("").trim();
                        self.glab_edit_buffer.push_str(clean);
                    }
                }
                None
            }
            KeyCode::Char(c) => {
                self.glab_edit_buffer.push(c);
                None
            }
            _ => None,
        }
    }

    fn glab_selected_action(&mut self) -> Option<SettingsAction> {
        match self.cursor {
            GLAB_ROW_REPO => {
                self.glab_editing = Some(GlabEditField::RepositoryUrl);
                self.glab_edit_buffer = self.glab_config.repository_url.clone().unwrap_or_default();
                None
            }
            GLAB_ROW_TOKEN => {
                self.glab_editing = Some(GlabEditField::Token);
                self.glab_edit_buffer = self.glab_config.token.clone().unwrap_or_default();
                None
            }
            GLAB_ROW_APPLY => Some(SettingsAction::ApplyGlabConfig(self.glab_config.clone())),
            GLAB_ROW_CLEAR => Some(SettingsAction::ClearGlabConfig),
            _ => None,
        }
    }

    fn glab_clear_field_at_cursor(&mut self) -> Option<SettingsAction> {
        match self.cursor {
            GLAB_ROW_REPO => {
                self.glab_config.repository_url = None;
                None
            }
            GLAB_ROW_TOKEN => {
                self.glab_config.token = None;
                None
            }
            _ => None,
        }
    }

    /// Snap the cursor to the "current" row on tab switch. For Agents
    /// that's row 0; for Viewer that's the row matching
    /// `self.markdown_theme` so opening the tab lands on the active
    /// theme (visual confirmation of what's applied).
    fn reset_cursor_for_tab(&mut self) {
        self.cursor = match self.tab {
            SettingsTab::Agents => 0,
            SettingsTab::Viewer => rimeterm_markdown::Theme::ALL
                .iter()
                .position(|t| *t == self.markdown_theme)
                .unwrap_or(0),
            SettingsTab::Shell => self
                .shells
                .iter()
                .position(|s| s.path() == self.current_shell.path())
                .unwrap_or(0),
            // Tabs: land on the first non-anchor row (row 1 within the
            // top group) so Shift+↑/Shift+↓ + Space discovery works
            // without a wasted key press on the locked anchor. Falls
            // back to 0 for the degenerate "top group has only the
            // anchor" case.
            SettingsTab::Tabs => {
                if self.left_tabs_state.top.len() > 1 {
                    1
                } else {
                    0
                }
            }
            // Integration: snap to Uninstall when already installed
            // so Enter defaults to the "toggle" action; otherwise
            // snap to Install.
            SettingsTab::Integration => {
                if self.integration_installed == Some(true) {
                    1
                } else {
                    0
                }
            }
            SettingsTab::Memory => 0,
            SettingsTab::Glab => 0,
        };
        self.glab_editing = None;
        self.glab_edit_buffer.clear();
    }

    /// Compute the popup rect for the current draw area. Extracted
    /// so App::on_mouse can hit-test the overlay without duplicating
    /// the sizing math from `render`.
    pub fn popup_rect(&self, area: Rect) -> Rect {
        let width = area.width.min(92).max(40);
        let height = area.height.min(28).max(8);
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.open {
            return;
        }
        let width = area.width.min(92).max(40);
        let height = area.height.min(28).max(8);
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        let popup = Rect {
            x,
            y,
            width,
            height,
        };
        Clear.render(popup, buf);
        let block = Block::default().title(" Settings ").borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);

        let accent = rimeterm_markdown::Palette::from_theme(self.markdown_theme).border_focused;
        let tab_line = Line::from(vec![
            Span::styled(
                " Agents ",
                tab_style(self.tab == SettingsTab::Agents, accent),
            ),
            Span::raw("  "),
            Span::styled(
                " Viewer ",
                tab_style(self.tab == SettingsTab::Viewer, accent),
            ),
            Span::raw("  "),
            Span::styled(" Shell ", tab_style(self.tab == SettingsTab::Shell, accent)),
            Span::raw("  "),
            Span::styled(" Tabs ", tab_style(self.tab == SettingsTab::Tabs, accent)),
            Span::raw("  "),
            Span::styled(
                " Integration ",
                tab_style(self.tab == SettingsTab::Integration, accent),
            ),
            Span::raw("  "),
            Span::styled(
                " Memory ",
                tab_style(self.tab == SettingsTab::Memory, accent),
            ),
            Span::raw("  "),
            Span::styled(" Glab ", tab_style(self.tab == SettingsTab::Glab, accent)),
            Span::styled(
                "   [Tab] switch",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]);
        Paragraph::new(tab_line).render(Rect { height: 1, ..inner }, buf);

        let body = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(2),
            ..inner
        };
        let mut lines = Vec::new();
        match self.tab {
            SettingsTab::Agents => {
                lines.push(Line::styled(
                    " ↑/↓ select   [Enter] open detected agent",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                for (idx, agent) in self.agents.iter().enumerate() {
                    let status = agent
                        .detected_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "missing".to_string());
                    let text = format!(" {:<18} {}", agent.label, status);
                    lines.push(Line::styled(
                        row_suffix(text, agent.is_available()),
                        row_style(idx == self.cursor),
                    ));
                }
            }
            SettingsTab::Viewer => {
                lines.push(Line::styled(
                    " ↑/↓ select   [Enter] apply theme to markdown viewer",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                for (idx, theme) in rimeterm_markdown::Theme::ALL.iter().enumerate() {
                    let marker = if *theme == self.markdown_theme {
                        "●"
                    } else {
                        " "
                    };
                    let text = format!(" {marker} {}", theme.label());
                    lines.push(Line::styled(text, row_style(idx == self.cursor)));
                }
            }
            SettingsTab::Shell => {
                lines.push(Line::styled(
                    " ↑/↓ select   [Enter] use for NEW shell tabs (existing shells unchanged)",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                if self.shells.is_empty() {
                    lines.push(Line::styled(
                        "  no shell detected — check [core].shell_win / shell_unix in config",
                        Style::default().fg(Color::Yellow),
                    ));
                } else {
                    for (idx, shell) in self.shells.iter().enumerate() {
                        let marker = if shell.path() == self.current_shell.path()
                            && shell.path().is_some()
                        {
                            "●"
                        } else {
                            " "
                        };
                        let path_str = shell
                            .path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(unresolved)".to_string());
                        let text = format!(" {marker} {:<16} {}", shell.display_label(), path_str);
                        lines.push(Line::styled(text, row_style(idx == self.cursor)));
                    }
                }
            }
            SettingsTab::Tabs => {
                lines.push(Line::styled(
                    " ↑/↓ move cursor · Shift+↑/↓ reorder · [Space] toggle · [Enter] toggle",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                lines.push(Line::styled(
                    "  Files and Git are pinned to position 1 in their column.",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                lines.push(Line::raw(""));
                self.render_tabs_group(&mut lines, "Left top (files column)", LeftGroup::Top, 0);
                lines.push(Line::raw(""));
                self.render_tabs_group(
                    &mut lines,
                    "Left bottom (git column)",
                    LeftGroup::Bottom,
                    self.left_tabs_state.top.len(),
                );
            }
            SettingsTab::Integration => {
                if !cfg!(windows) {
                    lines.push(Line::styled(
                        "  right-click integration is Windows-only for now",
                        Style::default().fg(Color::Yellow),
                    ));
                    lines.push(Line::styled(
                        "  (macOS / Linux integrations planned — track in the roadmap)",
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                } else {
                    lines.push(Line::styled(
                        " ↑/↓ select   [Enter] apply · adds \"Open with rimeterm here\" to Explorer",
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                    let status = match self.integration_installed {
                        Some(true) => {
                            Span::styled(" status: installed", Style::default().fg(Color::Green))
                        }
                        Some(false) => Span::styled(
                            " status: not installed",
                            Style::default().fg(Color::Yellow),
                        ),
                        None => Span::styled(
                            " status: unknown",
                            Style::default().add_modifier(Modifier::DIM),
                        ),
                    };
                    lines.push(Line::from(vec![status]));
                    lines.push(Line::raw(""));
                    let rows = [
                        ("Install context menu entry", 0usize),
                        ("Uninstall context menu entry", 1usize),
                    ];
                    for (label, idx) in rows {
                        let marker = match (idx, self.integration_installed) {
                            (1, Some(true)) => "●",
                            (0, Some(false)) => "●",
                            _ => " ",
                        };
                        let text = format!(" {marker} {label}");
                        lines.push(Line::styled(text, row_style(idx == self.cursor)));
                    }
                    lines.push(Line::raw(""));
                    lines.push(Line::styled(
                        "  writes HKCU\\Software\\Classes\\Directory\\... — no admin required",
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                }
            }
            SettingsTab::Memory => {
                lines.push(Line::styled(
                    " ↑/↓ select · [Space/Enter] remember on next launch",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                for (idx, label) in MEMORY_LABELS.iter().enumerate() {
                    let checkbox = if memory_policy_value(&self.memory_policy, idx) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    lines.push(Line::styled(
                        format!("  {checkbox} {label}"),
                        row_style(idx == self.cursor),
                    ));
                }
            }
            SettingsTab::Glab => {
                lines.push(Line::styled(
                    " ↑/↓ select · [Enter] edit · [d] clear field",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                // Row 0: Repository URL
                let repo_display = if self.glab_editing == Some(GlabEditField::RepositoryUrl) {
                    format!("  Repository:  {}_", self.glab_edit_buffer)
                } else {
                    let value = self
                        .glab_config
                        .repository_url
                        .as_deref()
                        .unwrap_or("(auto-detect from git remote)");
                    format!("  Repository:  {value}")
                };
                lines.push(Line::styled(
                    repo_display,
                    if self.glab_editing == Some(GlabEditField::RepositoryUrl) {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::UNDERLINED)
                    } else {
                        row_style(self.cursor == GLAB_ROW_REPO)
                    },
                ));
                // Row 1: Token
                let token_display = if self.glab_editing == Some(GlabEditField::Token) {
                    let masked: String =
                        std::iter::repeat_n('*', self.glab_edit_buffer.len()).collect();
                    format!("  Token:       {masked}_")
                } else {
                    let value = self
                        .glab_config
                        .token
                        .as_ref()
                        .map(|t| {
                            if t.len() <= 8 {
                                "****".to_string()
                            } else {
                                let visible = &t[..4];
                                format!("{visible}...****")
                            }
                        })
                        .unwrap_or_else(|| "(not set)".to_string());
                    format!("  Token:       {value}")
                };
                lines.push(Line::styled(
                    token_display,
                    if self.glab_editing == Some(GlabEditField::Token) {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::UNDERLINED)
                    } else {
                        row_style(self.cursor == GLAB_ROW_TOKEN)
                    },
                ));
                // Row 2: Apply & Reload
                lines.push(Line::styled(
                    "  [Apply & Reload]",
                    row_style(self.cursor == GLAB_ROW_APPLY),
                ));
                // Row 3: Clear & Auto-detect
                lines.push(Line::styled(
                    "  [Clear All & Auto-detect]",
                    row_style(self.cursor == GLAB_ROW_CLEAR),
                ));
                // Info section
                lines.push(Line::raw(""));
                let backend = self
                    .glab_config
                    .is_github()
                    .map(|g| if g { "GitHub (gh)" } else { "GitLab (glab)" })
                    .unwrap_or("(determined by repository URL)");
                lines.push(Line::styled(
                    format!("  Backend: {backend}"),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                lines.push(Line::styled(
                    "  Token is passed via GITLAB_TOKEN / GH_TOKEN env var to child processes.",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
        }
        if let Some(busy) = &self.busy {
            lines.push(Line::styled(
                format!("  ⏳ {busy}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        Paragraph::new(lines).render(body, buf);
    }

    /// Emit lines for one left-column group: a subheading followed by
    /// one row per catalog entry. `offset` = index in the flat cursor
    /// where this group's rows begin (0 for top, `top.len()` for
    /// bottom).
    fn render_tabs_group(
        &self,
        lines: &mut Vec<Line<'static>>,
        heading: &str,
        group: LeftGroup,
        offset: usize,
    ) {
        lines.push(Line::styled(
            format!(" {heading}"),
            Style::default().add_modifier(Modifier::UNDERLINED),
        ));
        let list = self.left_tabs_state_list(group);
        for (idx, tab) in list.iter().enumerate() {
            let checkbox = if tab.visible { "[x]" } else { "[ ]" };
            let label = self
                .left_tab_labels
                .get(&tab.id)
                .cloned()
                .unwrap_or_else(|| tab.id.clone());
            let position = idx + 1;
            let anchor_note = if is_anchor(group, &tab.id) {
                "  (locked)"
            } else {
                ""
            };
            let text = format!("  {position}. {checkbox} {label:<14}{anchor_note}");
            lines.push(Line::styled(text, row_style(offset + idx == self.cursor)));
        }
    }
}

// Glab tab row indices
const GLAB_ROW_REPO: usize = 0;
const GLAB_ROW_TOKEN: usize = 1;
const GLAB_ROW_APPLY: usize = 2;
const GLAB_ROW_CLEAR: usize = 3;
const GLAB_ROW_COUNT: usize = 4;

const MEMORY_LABELS: [&str; 16] = [
    "Last workspace directory",
    "Pane sizes",
    "Tab visibility and order",
    "Active tab in each group",
    "Agent tabs",
    "Shell tabs",
    "Files settings",
    "Git settings",
    "Todo settings",
    "Glab settings",
    "Fast Resume settings",
    "Sysmon settings",
    "Agtop settings",
    "Models settings",
    "Stock settings",
    "Zones settings",
];

fn memory_policy_value(policy: &rimeterm_config::memory_state::MemoryPolicy, index: usize) -> bool {
    match index {
        0 => policy.last_workspace,
        1 => policy.pane_sizes,
        2 => policy.tab_layout,
        3 => policy.active_tabs,
        4 => policy.agent_tabs,
        5 => policy.shell_tabs,
        6 => policy.files,
        7 => policy.git,
        8 => policy.todo,
        9 => policy.glab,
        10 => policy.fast_resume,
        11 => policy.sysmon,
        12 => policy.agtop,
        13 => policy.models,
        14 => policy.stock,
        15 => policy.zones,
        _ => false,
    }
}

fn toggle_memory_policy(
    policy: &mut rimeterm_config::memory_state::MemoryPolicy,
    index: usize,
) -> Option<()> {
    let value = match index {
        0 => &mut policy.last_workspace,
        1 => &mut policy.pane_sizes,
        2 => &mut policy.tab_layout,
        3 => &mut policy.active_tabs,
        4 => &mut policy.agent_tabs,
        5 => &mut policy.shell_tabs,
        6 => &mut policy.files,
        7 => &mut policy.git,
        8 => &mut policy.todo,
        9 => &mut policy.glab,
        10 => &mut policy.fast_resume,
        11 => &mut policy.sysmon,
        12 => &mut policy.agtop,
        13 => &mut policy.models,
        14 => &mut policy.stock,
        15 => &mut policy.zones,
        _ => return None,
    };
    *value = !*value;
    Some(())
}

fn is_anchor(group: LeftGroup, id: &str) -> bool {
    match group {
        LeftGroup::Top => id == rimeterm_config::left_tabs_state::ANCHOR_TOP,
        LeftGroup::Bottom => id == rimeterm_config::left_tabs_state::ANCHOR_BOTTOM,
    }
}

fn tab_style(active: bool, accent: Color) -> Style {
    if active {
        Style::default()
            .fg(accent)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}

fn row_style(active: bool) -> Style {
    if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    }
}

fn row_suffix(mut text: String, available: bool) -> String {
    if !available {
        text.push_str("  [not detected]");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn tabs_and_cursor_navigation_are_local() {
        let mut state = SettingsState::default();
        state.open = true;
        state.agents = Vec::new();
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Viewer);
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn escape_returns_close_action() {
        let mut state = SettingsState {
            open: true,
            ..Default::default()
        };
        assert_eq!(
            state.handle_key(key(KeyCode::Esc)),
            Some(SettingsAction::Close)
        );
    }

    #[test]
    fn tab_cycle_visits_every_tab() {
        // Agents → Viewer → Shell → Tabs → Integration → Memory → Glab → Agents
        let mut state = SettingsState::default();
        state.open = true;
        assert_eq!(state.tab, SettingsTab::Agents);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Viewer);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Shell);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Tabs);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Integration);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Memory);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Glab);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Agents);
    }

    #[test]
    fn left_arrow_wraps_from_agents_to_glab() {
        let mut state = SettingsState::default();
        state.open = true;
        assert_eq!(state.tab, SettingsTab::Agents);
        state.handle_key(key(KeyCode::Char('h')));
        assert_eq!(state.tab, SettingsTab::Glab);
    }

    #[test]
    fn viewer_tab_enter_returns_set_theme_action() {
        // Cursor at row 0 of Viewer tab → SetMarkdownTheme(Theme::ALL[0]).
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Viewer;
        state.cursor = 1; // Dracula
        let action = state.handle_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            Some(SettingsAction::SetMarkdownTheme(
                rimeterm_markdown::Theme::ALL[1]
            ))
        );
    }

    #[test]
    fn reset_cursor_for_viewer_lands_on_current_theme() {
        // set_markdown_theme(GruvboxDark) + switch to Viewer → cursor
        // snaps to GruvboxDark's index in Theme::ALL.
        let mut state = SettingsState::default();
        state.open = true;
        state.set_markdown_theme(rimeterm_markdown::Theme::GruvboxDark);
        // Move to Viewer tab via l (right).
        state.tab = SettingsTab::Agents;
        state.handle_key(key(KeyCode::Char('l'))); // Agents → Viewer
        assert_eq!(state.tab, SettingsTab::Viewer);
        let expected = rimeterm_markdown::Theme::ALL
            .iter()
            .position(|t| *t == rimeterm_markdown::Theme::GruvboxDark)
            .unwrap();
        assert_eq!(state.cursor, expected);
    }

    #[test]
    fn shell_tab_enter_returns_selected_executable() {
        let selected = ShellChoice::Unix(std::path::PathBuf::from("nu"));
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Shell;
        state.shells = vec![selected.clone()];

        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Some(SettingsAction::SetShell(selected))
        );
    }

    #[test]
    fn configured_shell_hints_drive_refresh() {
        let mut state = SettingsState::default();

        state.set_shell_hints(vec!["definitely-not-a-default-shell".into()]);

        assert_eq!(state.shell_hints, vec!["definitely-not-a-default-shell"]);
    }

    #[test]
    fn current_shell_remains_visible_when_not_on_path() {
        let current = ShellChoice::Unix(std::path::PathBuf::from("/opt/custom/nu"));
        let mut state = SettingsState::default();

        state.set_current_shell(current.clone());

        assert!(state.shells.contains(&current));
    }

    #[test]
    fn viewer_row_count_matches_theme_all_len() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Viewer;
        assert_eq!(state.row_count(), rimeterm_markdown::Theme::ALL.len());
    }

    #[cfg(windows)]
    #[test]
    fn integration_row_count_on_windows_is_two() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;
        assert_eq!(state.row_count(), 2);
    }

    #[cfg(not(windows))]
    #[test]
    fn integration_row_count_off_windows_is_zero() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;
        assert_eq!(state.row_count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn integration_enter_returns_install_when_cursor_zero() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;
        state.cursor = 0;
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Some(SettingsAction::InstallContextMenu)
        );
    }

    #[cfg(windows)]
    #[test]
    fn integration_enter_returns_uninstall_when_cursor_one() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;
        state.cursor = 1;
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Some(SettingsAction::UninstallContextMenu)
        );
    }

    #[cfg(windows)]
    #[test]
    fn integration_reset_cursor_prefers_uninstall_when_installed() {
        let mut state = SettingsState::default();
        state.open = true;
        state.set_integration_installed(Some(true));
        state.tab = SettingsTab::Tabs;
        state.handle_key(key(KeyCode::Char('l'))); // Tabs → Integration
        assert_eq!(state.tab, SettingsTab::Integration);
        assert_eq!(state.cursor, 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn integration_enter_off_windows_is_noop() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;
        state.cursor = 0;
        assert_eq!(state.handle_key(key(KeyCode::Enter)), None);
    }

    /// Seed a state matching the production catalog so tests can drive
    /// the Tabs panel with real ids without pulling in the whole App.
    fn seed_left_tabs(state: &mut SettingsState) {
        use rimeterm_config::left_tabs_state::{LeftTab, LeftTabsState};
        let mut s = LeftTabsState {
            top: vec![
                LeftTab::new("files", true),
                LeftTab::new("todo", true),
                LeftTab::new("fr", true),
            ],
            bottom: vec![
                LeftTab::new("git", true),
                LeftTab::new("glab", true),
                LeftTab::new("sysmon", true),
                LeftTab::new("agtop", true),
                LeftTab::new("models", true),
                LeftTab::new("stock", true),
            ],
        };
        s.normalize(
            &["files", "todo", "fr"],
            &["git", "glab", "sysmon", "agtop", "models", "stock"],
        );
        let labels = [
            ("files", "Files"),
            ("todo", "Tuxedo"),
            ("fr", "Fast Resume"),
            ("git", "Git"),
            ("glab", "Glab"),
            ("sysmon", "Sysmon"),
            ("agtop", "Agtop"),
            ("models", "Models"),
            ("stock", "Stock"),
        ]
        .into_iter()
        .map(|(id, label)| (id.to_string(), label.to_string()))
        .collect();
        state.set_left_tabs_state(s, labels);
    }

    #[test]
    fn tabs_panel_row_count_matches_state_sizes() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        // 3 top + 6 bottom = 9 rows.
        assert_eq!(state.row_count(), 9);
    }

    #[test]
    fn tabs_panel_reset_cursor_lands_on_first_non_anchor() {
        let mut state = SettingsState::default();
        state.open = true;
        seed_left_tabs(&mut state);
        state.tab = SettingsTab::Shell;
        state.handle_key(key(KeyCode::Char('l'))); // Shell → Tabs
        assert_eq!(state.tab, SettingsTab::Tabs);
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn tabs_panel_space_toggles_non_anchor_row() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        state.cursor = 1; // Todo row in the top group.
        let action = state.handle_key(key(KeyCode::Char(' ')));
        match action {
            Some(SettingsAction::SetLeftTabsState(s)) => {
                let todo = s.top.iter().find(|t| t.id == "todo").unwrap();
                assert!(!todo.visible, "Space should hide the row");
            }
            other => panic!("expected SetLeftTabsState, got {other:?}"),
        }
    }

    #[test]
    fn tabs_panel_space_rejects_anchor_row() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        state.cursor = 0; // Files anchor.
        assert_eq!(state.handle_key(key(KeyCode::Char(' '))), None);
        assert!(state.left_tabs_state.top[0].visible);
    }

    #[test]
    fn tabs_panel_shift_down_swaps_neighbors_within_group() {
        use crossterm::event::{KeyEventKind, KeyModifiers};
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        state.cursor = 1; // Todo row.
        let shift_down =
            KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::SHIFT, KeyEventKind::Press);
        let action = state.handle_key(shift_down);
        assert!(matches!(action, Some(SettingsAction::SetLeftTabsState(_))));
        // Todo moved to index 2; the row cursor followed it.
        assert_eq!(state.left_tabs_state.top[1].id, "fr");
        assert_eq!(state.left_tabs_state.top[2].id, "todo");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn tabs_panel_shift_up_refuses_to_swap_past_anchor() {
        use crossterm::event::{KeyEventKind, KeyModifiers};
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        state.cursor = 1; // Todo — sits immediately after Files.
        let shift_up =
            KeyEvent::new_with_kind(KeyCode::Up, KeyModifiers::SHIFT, KeyEventKind::Press);
        assert_eq!(state.handle_key(shift_up), None);
        // State unchanged: anchor still at 0, Todo still at 1.
        assert_eq!(state.left_tabs_state.top[0].id, "files");
        assert_eq!(state.left_tabs_state.top[1].id, "todo");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn tabs_panel_cursor_wraps_across_groups() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Tabs;
        seed_left_tabs(&mut state);
        state.cursor = 2; // Last row in top group (fr).
        state.handle_key(key(KeyCode::Down));
        assert_eq!(state.cursor, 3); // First row of bottom group (git anchor).
    }
    #[test]
    fn memory_tab_participates_in_navigation_cycle() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Integration;

        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Memory);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Glab);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Agents);
    }

    #[test]
    fn memory_panel_has_one_row_per_policy_category() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Memory;

        assert_eq!(state.row_count(), 16);
    }

    #[test]
    fn memory_panel_space_toggles_selected_category() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Memory;
        state.cursor = 1;

        let action = state.handle_key(key(KeyCode::Char(' ')));

        match action {
            Some(SettingsAction::SetMemoryPolicy(policy)) => {
                assert!(!policy.pane_sizes);
                assert!(policy.last_workspace);
            }
            other => panic!("expected SetMemoryPolicy, got {other:?}"),
        }
    }

    #[test]
    fn memory_panel_enter_toggles_selected_category() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Memory;
        state.cursor = 15;

        let action = state.handle_key(key(KeyCode::Enter));

        assert!(matches!(
            action,
            Some(SettingsAction::SetMemoryPolicy(policy)) if !policy.zones
        ));
    }
    #[test]
    fn memory_panel_renders_all_categories_in_normal_popup() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Memory;
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);

        state.render(area, &mut buffer);

        let rendered: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("Last workspace directory"));
        assert!(rendered.contains("Zones settings"));
        assert!(rendered.contains("Memory"));
    }

    #[test]
    fn memory_panel_small_terminal_does_not_panic() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Memory;
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);

        state.render(area, &mut buffer);
    }

    #[test]
    fn glab_tab_row_count() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        assert_eq!(state.row_count(), GLAB_ROW_COUNT);
    }

    #[test]
    fn glab_tab_enter_on_repo_starts_edit() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.cursor = GLAB_ROW_REPO;
        let action = state.handle_key(key(KeyCode::Enter));
        assert!(action.is_none());
        assert_eq!(state.glab_editing, Some(GlabEditField::RepositoryUrl));
    }

    #[test]
    fn glab_tab_edit_mode_captures_text() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.cursor = GLAB_ROW_REPO;
        state.handle_key(key(KeyCode::Enter)); // start edit
        state.handle_key(key(KeyCode::Char('h')));
        state.handle_key(key(KeyCode::Char('i')));
        assert_eq!(state.glab_edit_buffer, "hi");
        state.handle_key(key(KeyCode::Backspace));
        assert_eq!(state.glab_edit_buffer, "h");
        state.handle_key(key(KeyCode::Enter)); // confirm
        assert_eq!(state.glab_editing, None);
        assert_eq!(state.glab_config.repository_url.as_deref(), Some("h"));
    }

    #[test]
    fn glab_tab_edit_esc_cancels() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.glab_config.repository_url = Some("original".into());
        state.cursor = GLAB_ROW_REPO;
        state.handle_key(key(KeyCode::Enter)); // start edit
        state.handle_key(key(KeyCode::Char('x')));
        state.handle_key(key(KeyCode::Esc)); // cancel
        assert_eq!(state.glab_editing, None);
        assert_eq!(
            state.glab_config.repository_url.as_deref(),
            Some("original")
        );
    }

    #[test]
    fn glab_tab_d_clears_repo() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.glab_config.repository_url = Some("https://gitlab.com/x/y".into());
        state.cursor = GLAB_ROW_REPO;
        state.handle_key(key(KeyCode::Char('d')));
        assert!(state.glab_config.repository_url.is_none());
    }

    #[test]
    fn glab_tab_apply_returns_config() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.glab_config.repository_url = Some("https://github.com/a/b".into());
        state.cursor = GLAB_ROW_APPLY;
        let action = state.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(SettingsAction::ApplyGlabConfig(_))));
    }

    #[test]
    fn glab_tab_clear_returns_action() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        state.cursor = GLAB_ROW_CLEAR;
        let action = state.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Some(SettingsAction::ClearGlabConfig));
    }

    #[test]
    fn glab_tab_renders_without_panic() {
        let mut state = SettingsState::default();
        state.open = true;
        state.tab = SettingsTab::Glab;
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        state.render(area, &mut buffer);
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Repository"));
        assert!(text.contains("Token"));
    }
}
