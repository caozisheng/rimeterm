//! Modal picker overlay — a short list of labeled choices bound to
//! either a registered command id **or** a caller-defined "intent" string
//! that the app matches on. Simpler than the command palette: no fuzzy
//! filter, no description column, no scrolling above ~20 entries.
//!
//! Two entry points into rimeterm today:
//!
//! - `[+]` on the agents tab strip / Ctrl+T on agents / click on the
//!   "Pick an agent" placeholder → agent-registry-driven dropdown.
//! - Right-click on anything → context menu (mix of Command + Intent
//!   entries so the same menu can call registered commands AND perform
//!   click-target-specific actions).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_core::command::CommandId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Fire a registered command from the CommandRegistry by id.
    Command(CommandId),
    /// Trigger a caller-defined intent — the picker consumer (App) matches
    /// on the string. Kept as `String` so the picker module stays free of
    /// App-specific types.
    Intent(String),
    /// Disabled row — cursor skips it, renders dim.
    Disabled,
}

impl PickerAction {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, PickerAction::Disabled)
    }
}

/// One row in the picker.
#[derive(Debug, Clone)]
pub struct PickerEntry {
    /// Primary label (e.g. `Oh-my-pi`).
    pub label: String,
    /// Optional grey right-aligned annotation (e.g. `not installed`).
    pub note: Option<String>,
    /// What Enter does on this row.
    pub action: PickerAction,
}

impl PickerEntry {
    pub fn command(label: impl Into<String>, cmd: CommandId) -> Self {
        Self {
            label: label.into(),
            note: None,
            action: PickerAction::Command(cmd),
        }
    }
    pub fn intent(label: impl Into<String>, intent: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            note: None,
            action: PickerAction::Intent(intent.into()),
        }
    }
    pub fn disabled(label: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            note: Some(note.into()),
            action: PickerAction::Disabled,
        }
    }
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

#[derive(Debug, Default, Clone)]
pub struct PickerState {
    pub open: bool,
    pub title: String,
    pub entries: Vec<PickerEntry>,
    pub cursor: usize,
}

impl PickerState {
    pub fn open_with(&mut self, title: impl Into<String>, entries: Vec<PickerEntry>) {
        self.open = true;
        self.title = title.into();
        self.entries = entries;
        self.cursor = first_enabled(&self.entries).unwrap_or(0);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.entries.clear();
        self.cursor = 0;
    }

    /// Move cursor by `step` (positive = down, negative = up), skipping
    /// disabled rows. Wraps at both ends.
    pub fn step(&mut self, step: isize) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut idx = self.cursor;
        for _ in 0..n {
            idx = ((idx as isize + step).rem_euclid(n as isize)) as usize;
            if self.entries[idx].action.is_enabled() {
                self.cursor = idx;
                return;
            }
        }
    }

    /// Snapshot the action at the cursor (cloned so callers can drive App
    /// state changes without holding a picker borrow).
    pub fn selected_action(&self) -> Option<PickerAction> {
        self.entries
            .get(self.cursor)
            .map(|e| e.action.clone())
            .filter(|a| a.is_enabled())
    }
}

fn first_enabled(entries: &[PickerEntry]) -> Option<usize> {
    entries.iter().position(|e| e.action.is_enabled())
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerOutcome {
    Consumed,
    Closed,
    Run(PickerAction),
}

pub fn handle_key(state: &mut PickerState, key: KeyEvent) -> PickerOutcome {
    match key.code {
        KeyCode::Esc => {
            state.close();
            PickerOutcome::Closed
        }
        KeyCode::Up => {
            state.step(-1);
            PickerOutcome::Consumed
        }
        KeyCode::Down => {
            state.step(1);
            PickerOutcome::Consumed
        }
        KeyCode::Enter => {
            if let Some(action) = state.selected_action() {
                state.close();
                PickerOutcome::Run(action)
            } else {
                PickerOutcome::Consumed
            }
        }
        _ => PickerOutcome::Consumed,
    }
}

/// Centered popup sized to fit the entries; caps at a third of the terminal
/// so it doesn't crowd out the shell/agents panes behind it.
pub fn popup_rect(area: Rect, state: &PickerState) -> Rect {
    use unicode_width::UnicodeWidthStr;
    let content_w = state
        .entries
        .iter()
        .map(|e| {
            let label = UnicodeWidthStr::width(e.label.as_str()) as u16;
            let note = e.note.as_deref().map(UnicodeWidthStr::width).unwrap_or(0) as u16;
            label + if note > 0 { note + 3 } else { 0 }
        })
        .max()
        .unwrap_or(0);
    let title_w = UnicodeWidthStr::width(state.title.as_str()) as u16 + 4;
    let width = content_w.max(title_w).saturating_add(4).clamp(28, 64);
    let width = width.min(area.width.saturating_sub(4));
    let height = ((state.entries.len() as u16) + 2)
        .clamp(4, 18)
        .min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

pub fn render(area: Rect, buf: &mut Buffer, state: &PickerState) {
    if !state.open {
        return;
    }
    Clear.render(area, buf);
    let title = format!(" ▾ {} ", state.title);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(area);
    block.render(area, buf);

    let disabled = Style::default().add_modifier(Modifier::DIM);
    let selected = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
    let note_style = Style::default().fg(Color::DarkGray);

    for (idx, entry) in state.entries.iter().take(inner.height as usize).enumerate() {
        let row = Rect {
            x: inner.x,
            y: inner.y + idx as u16,
            width: inner.width,
            height: 1,
        };
        let mut spans: Vec<Span<'_>> = Vec::new();
        let prefix = if idx == state.cursor && entry.action.is_enabled() {
            "▶ "
        } else {
            "  "
        };
        let base_style = if !entry.action.is_enabled() {
            disabled
        } else if idx == state.cursor {
            selected
        } else {
            Style::default()
        };
        spans.push(Span::styled(prefix, base_style));
        spans.push(Span::styled(entry.label.clone(), base_style));
        if let Some(note) = &entry.note {
            spans.push(Span::styled(format!("  {}", note), note_style));
        }
        Paragraph::new(Line::from(spans)).render(row, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn open_with_lands_cursor_on_first_enabled() {
        let mut s = PickerState::default();
        s.open_with(
            "T",
            vec![
                PickerEntry::disabled("disabled-1", "n/a"),
                PickerEntry::disabled("disabled-2", "n/a"),
                PickerEntry::command("live-1", "app.quit"),
                PickerEntry::command("live-2", "app.settings"),
            ],
        );
        assert_eq!(s.cursor, 2);
        assert!(matches!(
            s.selected_action(),
            Some(PickerAction::Command("app.quit"))
        ));
    }

    #[test]
    fn step_skips_disabled_and_wraps() {
        let mut s = PickerState::default();
        s.open_with(
            "T",
            vec![
                PickerEntry::command("a", "a"),
                PickerEntry::disabled("dead", "n/a"),
                PickerEntry::command("b", "b"),
                PickerEntry::command("c", "c"),
            ],
        );
        assert_eq!(s.cursor, 0);
        s.step(1);
        assert!(matches!(
            s.selected_action(),
            Some(PickerAction::Command("b"))
        ));
        s.step(1);
        assert!(matches!(
            s.selected_action(),
            Some(PickerAction::Command("c"))
        ));
        s.step(1);
        assert!(matches!(
            s.selected_action(),
            Some(PickerAction::Command("a"))
        ));
        s.step(-1);
        assert!(matches!(
            s.selected_action(),
            Some(PickerAction::Command("c"))
        ));
    }

    #[test]
    fn enter_on_command_row_returns_run() {
        let mut s = PickerState::default();
        s.open_with("T", vec![PickerEntry::command("a", "cmd.a")]);
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter)),
            PickerOutcome::Run(PickerAction::Command("cmd.a"))
        );
        assert!(!s.open);
    }

    #[test]
    fn enter_on_intent_row_returns_run_intent() {
        let mut s = PickerState::default();
        s.open_with("T", vec![PickerEntry::intent("close this", "tab.close:42")]);
        match handle_key(&mut s, key(KeyCode::Enter)) {
            PickerOutcome::Run(PickerAction::Intent(s)) => assert_eq!(s, "tab.close:42"),
            other => panic!("expected Intent, got {other:?}"),
        }
        assert!(!s.open);
    }

    #[test]
    fn enter_on_disabled_is_noop() {
        let mut s = PickerState::default();
        s.open_with("T", vec![PickerEntry::disabled("a", "n/a")]);
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter)),
            PickerOutcome::Consumed
        );
        assert!(s.open);
    }

    #[test]
    fn esc_closes() {
        let mut s = PickerState::default();
        s.open_with("T", vec![PickerEntry::command("a", "a")]);
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), PickerOutcome::Closed);
        assert!(!s.open);
    }
}
