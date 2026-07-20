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

/// Where the popup should land on screen. `Centered` is the default (used
/// by the agent picker on `Ctrl+T`); `Anchored` pins the popup near a
/// specific `(col, row)` inside a bounding rect, so a right-click context
/// menu drops next to the click and stays inside the pane it came from.
#[derive(Debug, Clone, Copy, Default)]
pub enum PickerAnchor {
    /// Center on the given draw area (usually the whole terminal rect).
    #[default]
    Centered,
    /// Anchor so the top-left corner sits at `(x, y)`, clipping the popup
    /// to fit inside `bounds`. The popup grows down-right by default;
    /// if that would overflow it flips up / left. Used by the right-click
    /// context menu — `bounds` is typically the clicked pane's outer rect.
    Anchored { x: u16, y: u16, bounds: Rect },
}

#[derive(Debug, Default, Clone)]
pub struct PickerState {
    pub open: bool,
    pub title: String,
    pub entries: Vec<PickerEntry>,
    pub cursor: usize,
    pub anchor: PickerAnchor,
}

impl PickerState {
    /// Open centered on the screen (default behavior).
    pub fn open_with(&mut self, title: impl Into<String>, entries: Vec<PickerEntry>) {
        self.open_with_anchor(title, entries, PickerAnchor::Centered);
    }

    /// Open with an explicit anchor (used by the right-click context menu).
    pub fn open_with_anchor(
        &mut self,
        title: impl Into<String>,
        entries: Vec<PickerEntry>,
        anchor: PickerAnchor,
    ) {
        self.open = true;
        self.title = title.into();
        self.entries = entries;
        self.cursor = first_enabled(&self.entries).unwrap_or(0);
        self.anchor = anchor;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.entries.clear();
        self.cursor = 0;
        self.anchor = PickerAnchor::Centered;
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

/// Compute the popup rect. Layout: measure content, clamp to a sensible
/// max, then place per [`PickerAnchor`]:
///
/// - `Centered`: middle of `area`.
/// - `Anchored { x, y, bounds }`: drop the popup's top-left at `(x + 1,
///   y + 1)` if it fits under-right of the click; otherwise flip up
///   and/or left. Always clipped inside `bounds` so a context menu on
///   the shell pane stays over the shell pane.
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
    let ideal_w = content_w.max(title_w).saturating_add(4).clamp(28, 64);
    let ideal_h = ((state.entries.len() as u16) + 2).clamp(4, 18);

    match state.anchor {
        PickerAnchor::Centered => {
            let width = ideal_w.min(area.width.saturating_sub(4));
            let height = ideal_h.min(area.height.saturating_sub(2));
            let x = area.x + area.width.saturating_sub(width) / 2;
            let y = area.y + area.height.saturating_sub(height) / 2;
            Rect {
                x,
                y,
                width,
                height,
            }
        }
        PickerAnchor::Anchored {
            x: click_x,
            y: click_y,
            bounds,
        } => {
            // Clip max size to the bounding pane; a picker larger than the
            // pane would look wrong even if it fits the screen.
            let width = ideal_w.min(bounds.width.saturating_sub(2)).max(4);
            let height = ideal_h.min(bounds.height.saturating_sub(2)).max(3);
            let right_edge = bounds.x.saturating_add(bounds.width);
            let bottom_edge = bounds.y.saturating_add(bounds.height);
            // Prefer under-right of the click; flip if we'd overflow.
            let mut x = click_x.saturating_add(1);
            if x + width > right_edge {
                // Flip left of the click.
                x = click_x.saturating_sub(width);
            }
            // Final clamp to bounds — covers the case where flipping
            // still overflows (rare, small pane).
            let x = x
                .max(bounds.x)
                .min(right_edge.saturating_sub(width).max(bounds.x));
            let mut y = click_y.saturating_add(1);
            if y + height > bottom_edge {
                y = click_y.saturating_sub(height);
            }
            let y = y
                .max(bounds.y)
                .min(bottom_edge.saturating_sub(height).max(bounds.y));
            Rect {
                x,
                y,
                width,
                height,
            }
        }
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

    // --- popup_rect anchor placement ---

    fn state_with(anchor: PickerAnchor, entries: usize) -> PickerState {
        let mut s = PickerState::default();
        let list: Vec<PickerEntry> = (0..entries)
            .map(|i| PickerEntry::command(format!("row-{i}"), "cmd.x"))
            .collect();
        s.open_with_anchor("T", list, anchor);
        s
    }

    #[test]
    fn centered_anchor_lands_in_middle_of_area() {
        let s = state_with(PickerAnchor::Centered, 5);
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let r = popup_rect(area, &s);
        assert!(r.x > 20 && r.x < 100, "x={}", r.x);
        assert!(r.y > 5 && r.y < 30, "y={}", r.y);
    }

    #[test]
    fn anchored_drops_down_right_of_click_when_it_fits() {
        let bounds = Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        };
        let s = state_with(
            PickerAnchor::Anchored {
                x: 20,
                y: 8,
                bounds,
            },
            5,
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let r = popup_rect(area, &s);
        // Preferred position: (click_x + 1, click_y + 1).
        assert_eq!(r.x, 21);
        assert_eq!(r.y, 9);
        // Still inside bounds.
        assert!(r.x + r.width <= bounds.x + bounds.width);
        assert!(r.y + r.height <= bounds.y + bounds.height);
    }

    #[test]
    fn anchored_flips_left_when_right_would_overflow() {
        // Click near right edge — down-right doesn't fit, must flip left.
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        let s = state_with(
            PickerAnchor::Anchored {
                x: 38,
                y: 5,
                bounds,
            },
            5,
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        let r = popup_rect(area, &s);
        // The popup's right edge should be inside bounds.
        assert!(
            r.x + r.width <= bounds.x + bounds.width,
            "r.x + r.width = {} > bounds.width = {}",
            r.x + r.width,
            bounds.width
        );
    }

    #[test]
    fn anchored_flips_up_when_below_would_overflow() {
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let s = state_with(
            PickerAnchor::Anchored {
                x: 5,
                y: 18,
                bounds,
            },
            5,
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let r = popup_rect(area, &s);
        assert!(
            r.y + r.height <= bounds.y + bounds.height,
            "r.y + r.height = {} > bounds.height = {}",
            r.y + r.height,
            bounds.height
        );
    }

    #[test]
    fn anchored_clamps_size_inside_tight_bounds() {
        // Bounds smaller than the picker's ideal size — width/height should
        // shrink to fit rather than overflow.
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let s = state_with(PickerAnchor::Anchored { x: 2, y: 1, bounds }, 5);
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let r = popup_rect(area, &s);
        assert!(r.width <= bounds.width);
        assert!(r.height <= bounds.height);
    }
}
