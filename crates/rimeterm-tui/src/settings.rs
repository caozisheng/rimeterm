//! Native Settings overlay for the Tools and Agents registries (C19).
//!
//! The overlay owns only presentation state. Tool actions are returned to App,
//! which schedules them away from the render loop; agent selection is likewise
//! handled by App so this module never owns pane or command handles.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_config::tools::DetectedTool;
use rimeterm_pty::ShellChoice;
use rimeterm_pty::agent_registry::DetectedAgent;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Tools,
    Agents,
    /// C22.6: Alt+V viewer knobs. Currently only the markdown theme
    /// picker lives here.
    Viewer,
    /// System shell picker: choose which shell (`pwsh` / `powershell` /
    /// `cmd` / `bash` / `fish` / …) is spawned when the user opens a
    /// new shell tab. Rows are populated by
    /// [`rimeterm_pty::detect_all_shells`].
    Shell,
    /// OS-shell integration: install / uninstall the "Open with
    /// rimeterm here" right-click entry on Explorer folder + folder
    /// background. Windows-only for now (writes HKCU registry
    /// entries — no admin needed); other platforms render a
    /// "not supported" notice.
    Integration,
}

impl Default for SettingsTab {
    fn default() -> Self {
        Self::Tools
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ToolAction {
    Install,
    Upgrade,
    Uninstall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsAction {
    Tool {
        name: String,
        action: ToolAction,
    },
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
    Refresh,
    Close,
}

#[derive(Debug)]
pub struct SettingsState {
    pub open: bool,
    pub tab: SettingsTab,
    pub cursor: usize,
    pub tools: Vec<DetectedTool>,
    pub agents: Vec<DetectedAgent>,
    /// Populated by [`Self::refresh`] on open. Empty if the host has
    /// no shells at all (extremely unusual).
    pub shells: Vec<ShellChoice>,
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
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            open: false,
            tab: SettingsTab::default(),
            cursor: 0,
            tools: Vec::new(),
            agents: Vec::new(),
            shells: Vec::new(),
            busy: None,
            markdown_theme: rimeterm_markdown::Theme::default(),
            current_shell: ShellChoice::None,
            integration_installed: None,
        }
    }
}

impl SettingsState {
    pub fn open(&mut self) {
        self.open = true;
        self.tab = SettingsTab::Tools;
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
        self.current_shell = shell;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.busy = None;
    }

    pub fn refresh(&mut self) {
        self.tools = rimeterm_config::tools::detect_all();
        self.agents = rimeterm_pty::agent_registry::detect_all();
        // Same hint list App uses to build its own initial choice, so
        // the picker rows always contain the currently-active shell.
        let hints: &[String] = if cfg!(windows) {
            &rimeterm_config::CoreConfig::default().shell_win
        } else {
            &rimeterm_config::CoreConfig::default().shell_unix
        };
        self.shells = rimeterm_pty::detect_all_shells(hints);
        self.integration_installed = crate::shell_integration::probe();
        self.cursor = self.cursor.min(self.row_count().saturating_sub(1));
    }

    /// Reflect the outcome of an install / uninstall action so the
    /// Integration tab's status marker updates without a full refresh.
    /// Called by `App::apply_settings_action`.
    pub fn set_integration_installed(&mut self, installed: Option<bool>) {
        self.integration_installed = installed;
    }

    fn row_count(&self) -> usize {
        match self.tab {
            SettingsTab::Tools => self.tools.len(),
            SettingsTab::Agents => self.agents.len(),
            SettingsTab::Viewer => rimeterm_markdown::Theme::ALL.len(),
            SettingsTab::Shell => self.shells.len(),
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

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SettingsAction> {
        if !self.open {
            return None;
        }
        match key.code {
            KeyCode::Esc => Some(SettingsAction::Close),
            KeyCode::Tab => {
                // Cycle Tools → Agents → Viewer → Shell → Integration → Tools.
                self.tab = match self.tab {
                    SettingsTab::Tools => SettingsTab::Agents,
                    SettingsTab::Agents => SettingsTab::Viewer,
                    SettingsTab::Viewer => SettingsTab::Shell,
                    SettingsTab::Shell => SettingsTab::Integration,
                    SettingsTab::Integration => SettingsTab::Tools,
                };
                self.reset_cursor_for_tab();
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.tab = match self.tab {
                    SettingsTab::Tools => SettingsTab::Integration,
                    SettingsTab::Agents => SettingsTab::Tools,
                    SettingsTab::Viewer => SettingsTab::Agents,
                    SettingsTab::Shell => SettingsTab::Viewer,
                    SettingsTab::Integration => SettingsTab::Shell,
                };
                self.reset_cursor_for_tab();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab = match self.tab {
                    SettingsTab::Tools => SettingsTab::Agents,
                    SettingsTab::Agents => SettingsTab::Viewer,
                    SettingsTab::Viewer => SettingsTab::Shell,
                    SettingsTab::Shell => SettingsTab::Integration,
                    SettingsTab::Integration => SettingsTab::Tools,
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
            KeyCode::Char('i') | KeyCode::Char('I') => self.tool_action(ToolAction::Install),
            KeyCode::Char('u') | KeyCode::Char('U') => self.tool_action(ToolAction::Upgrade),
            KeyCode::Char('x') | KeyCode::Char('X') => self.tool_action(ToolAction::Uninstall),
            _ => None,
        }
    }

    fn selected_action(&self) -> Option<SettingsAction> {
        match self.tab {
            SettingsTab::Tools => self.tool_action(ToolAction::Install),
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
        }
    }

    /// Snap the cursor to the "current" row on tab switch. For Tools
    /// / Agents that's row 0; for Viewer that's the row matching
    /// `self.markdown_theme` so opening the tab lands on the active
    /// theme (visual confirmation of what's applied).
    fn reset_cursor_for_tab(&mut self) {
        self.cursor = match self.tab {
            SettingsTab::Tools | SettingsTab::Agents => 0,
            SettingsTab::Viewer => rimeterm_markdown::Theme::ALL
                .iter()
                .position(|t| *t == self.markdown_theme)
                .unwrap_or(0),
            SettingsTab::Shell => self
                .shells
                .iter()
                .position(|s| s.path() == self.current_shell.path())
                .unwrap_or(0),
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
        };
    }

    fn tool_action(&self, action: ToolAction) -> Option<SettingsAction> {
        self.tools
            .get(self.cursor)
            .map(|tool| SettingsAction::Tool {
                name: tool.name.to_string(),
                action,
            })
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
        let block = Block::default()
            .title(" Settings · Tools / Agents / Viewer / Shell / Integration ")
            .borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);

        let accent = rimeterm_markdown::Palette::from_theme(self.markdown_theme).border_focused;
        let tab_line = Line::from(vec![
            Span::styled(" Tools ", tab_style(self.tab == SettingsTab::Tools, accent)),
            Span::raw("  "),
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
            Span::styled(
                " Integration ",
                tab_style(self.tab == SettingsTab::Integration, accent),
            ),
            Span::styled(
                "   [Tab] switch · [r] refresh",
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
            SettingsTab::Tools => {
                lines.push(Line::styled(
                    " ↑/↓ select   [I]nstall [U]pgrade [X] Uninstall",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                for (idx, tool) in self.tools.iter().enumerate() {
                    let source = format!("{:?}", tool.install_source).to_ascii_lowercase();
                    let status = tool
                        .detected_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "missing".to_string());
                    let text = format!(" {:<10} {:<9} {}", tool.name, source, status);
                    lines.push(Line::styled(text, row_style(idx == self.cursor)));
                }
            }
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
                        Some(true) => Span::styled(
                            " status: installed",
                            Style::default().fg(Color::Green),
                        ),
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
        }
        if let Some(busy) = &self.busy {
            lines.push(Line::styled(
                format!("  ⏳ {busy}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        Paragraph::new(lines).render(body, buf);
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
        state.tools = Vec::new();
        state.agents = Vec::new();
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Agents);
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
        // Tools → Agents → Viewer → Shell → Integration → Tools
        let mut state = SettingsState::default();
        state.open = true;
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Agents);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Viewer);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Shell);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Integration);
        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.tab, SettingsTab::Tools);
    }

    #[test]
    fn left_arrow_wraps_from_tools_to_integration() {
        let mut state = SettingsState::default();
        state.open = true;
        state.handle_key(key(KeyCode::Char('h')));
        assert_eq!(state.tab, SettingsTab::Integration);
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
        state.tab = SettingsTab::Shell;
        state.handle_key(key(KeyCode::Char('l'))); // Shell → Integration
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
}
