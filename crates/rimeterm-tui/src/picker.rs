//! Modal picker overlay — a short list of labeled choices bound to
//! command ids. Simpler than the command palette: no fuzzy filter, no
//! description column, no scrolling above ~20 entries. Reused by:
//!
//! - `[+]` on the agents tab strip (choose which agent to spawn),
//! - clicks on the "Pick an agent" placeholder pane,
//! - right-click context menus (§ item 2/3).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_core::command::CommandId;

/// One row in the picker.
#[derive(Debug, Clone)]
pub struct PickerEntry {
    /// Primary label (e.g. `Oh-my-pi`).
    pub label: String,
    /// Optional grey right-aligned annotation (e.g. `not installed`).
    pub note: Option<String>,
    /// The command id fired when the user hits Enter on this row.
    /// `None` disables the row (renders dim, cursor skips it).
    pub command: Option<CommandId>,
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
        // Land the cursor on the first enabled row.
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
            if self.entries[idx].command.is_some() {
                self.cursor = idx;
                return;
            }
        }
        // All disabled — leave cursor where it was.
    }

    /// Return the command id at the cursor if enabled.
    pub fn selected_command(&self) -> Option<CommandId> {
        self.entries.get(self.cursor).and_then(|e| e.command)
    }
}

fn first_enabled(entries: &[PickerEntry]) -> Option<usize> {
    entries.iter().position(|e| e.command.is_some())
}

#[derive(Debug, PartialEq)]
pub enum PickerOutcome {
    Consumed,
    Closed,
    Run(CommandId),
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
            if let Some(cmd) = state.selected_command() {
                state.close();
                PickerOutcome::Run(cmd)
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
            let note = e
                .note
                .as_deref()
                .map(UnicodeWidthStr::width)
                .unwrap_or(0) as u16;
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
    Rect { x, y, width, height }
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
        let prefix = if idx == state.cursor { "▶ " } else { "  " };
        let base_style = if entry.command.is_none() {
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

    fn entry(label: &str, cmd: Option<CommandId>) -> PickerEntry {
        PickerEntry { label: label.into(), note: None, command: cmd }
    }

    #[test]
    fn open_with_lands_cursor_on_first_enabled() {
        let mut s = PickerState::default();
        s.open_with(
            "T",
            vec![
                entry("disabled-1", None),
                entry("disabled-2", None),
                entry("live-1", Some("app.quit")),
                entry("live-2", Some("app.settings")),
            ],
        );
        assert_eq!(s.cursor, 2);
        assert_eq!(s.selected_command(), Some("app.quit"));
    }

    #[test]
    fn step_skips_disabled_and_wraps() {
        let mut s = PickerState::default();
        s.open_with(
            "T",
            vec![
                entry("a", Some("a")),
                entry("dead", None),
                entry("b", Some("b")),
                entry("c", Some("c")),
            ],
        );
        assert_eq!(s.cursor, 0);
        s.step(1); // → skip 'dead', land on 'b'
        assert_eq!(s.selected_command(), Some("b"));
        s.step(1); // → 'c'
        assert_eq!(s.selected_command(), Some("c"));
        s.step(1); // → wrap to 'a'
        assert_eq!(s.selected_command(), Some("a"));
        s.step(-1); // → wrap backwards to 'c'
        assert_eq!(s.selected_command(), Some("c"));
    }

    #[test]
    fn enter_on_enabled_returns_run() {
        let mut s = PickerState::default();
        s.open_with("T", vec![entry("a", Some("cmd.a"))]);
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), PickerOutcome::Run("cmd.a"));
        assert!(!s.open);
    }

    #[test]
    fn enter_on_disabled_is_noop() {
        let mut s = PickerState::default();
        s.open_with("T", vec![entry("a", None)]);
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), PickerOutcome::Consumed);
        assert!(s.open);
    }

    #[test]
    fn esc_closes() {
        let mut s = PickerState::default();
        s.open_with("T", vec![entry("a", Some("a"))]);
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), PickerOutcome::Closed);
        assert!(!s.open);
    }
}
