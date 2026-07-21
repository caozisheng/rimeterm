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
use rimeterm_pty::agent_registry::DetectedAgent;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Tools,
    Agents,
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
    Tool { name: String, action: ToolAction },
    Agent { id: String },
    Refresh,
    Close,
}

#[derive(Debug, Default)]
pub struct SettingsState {
    pub open: bool,
    pub tab: SettingsTab,
    pub cursor: usize,
    pub tools: Vec<DetectedTool>,
    pub agents: Vec<DetectedAgent>,
    pub busy: Option<String>,
}

impl SettingsState {
    pub fn open(&mut self) {
        self.open = true;
        self.tab = SettingsTab::Tools;
        self.cursor = 0;
        self.refresh();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.busy = None;
    }

    pub fn refresh(&mut self) {
        self.tools = rimeterm_config::tools::detect_all();
        self.agents = rimeterm_pty::agent_registry::detect_all();
        self.cursor = self.cursor.min(self.row_count().saturating_sub(1));
    }

    fn row_count(&self) -> usize {
        match self.tab {
            SettingsTab::Tools => self.tools.len(),
            SettingsTab::Agents => self.agents.len(),
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
                self.tab = match self.tab {
                    SettingsTab::Tools => SettingsTab::Agents,
                    SettingsTab::Agents => SettingsTab::Tools,
                };
                self.cursor = 0;
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.tab = SettingsTab::Tools;
                self.cursor = 0;
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab = SettingsTab::Agents;
                self.cursor = 0;
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
        }
    }

    fn tool_action(&self, action: ToolAction) -> Option<SettingsAction> {
        self.tools
            .get(self.cursor)
            .map(|tool| SettingsAction::Tool {
                name: tool.name.to_string(),
                action,
            })
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
            .title(" Settings · Tools / Agents ")
            .borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);

        let tab_line = Line::from(vec![
            Span::styled(" Tools ", tab_style(self.tab == SettingsTab::Tools)),
            Span::raw("  "),
            Span::styled(" Agents ", tab_style(self.tab == SettingsTab::Agents)),
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

fn tab_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
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
}
