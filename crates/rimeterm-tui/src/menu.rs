//! App-menu popover (§19.13). Overlay pane; does not enter the split tree.
//!
//! Very small state machine:
//! - `Closed`   → hidden
//! - `Open{i}`  → shown, index `i` highlighted; ↑/↓ / j/k move, Enter fires
//!                the item's command, Esc closes.
//!
//! Command execution is delegated to the caller: this module returns the
//! [`CommandId`] to fire; whoever owns the [`CommandRegistry`] runs it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_core::app_menu::AppMenu;
use rimeterm_core::command::CommandId;

#[derive(Debug, Default)]
pub struct MenuState {
    pub open: bool,
    pub cursor: usize,
}

impl MenuState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            self.cursor = 0;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.cursor = 0;
    }
}

/// Outcome of a key press while the menu is open.
#[derive(Debug, PartialEq)]
pub enum MenuKeyOutcome {
    /// Do nothing; keep the popover open.
    Consumed,
    /// Popover chose to close (Esc, click-away).
    Closed,
    /// User activated an item; run this command then close.
    Run(CommandId),
    /// Not a menu key; caller should route elsewhere.
    Passthrough,
}

pub fn handle_key(state: &mut MenuState, menu: &AppMenu, key: KeyEvent) -> MenuKeyOutcome {
    if !state.open {
        return MenuKeyOutcome::Passthrough;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::F(10), _) => {
            state.close();
            MenuKeyOutcome::Closed
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            state.cursor = state.cursor.saturating_sub(1);
            MenuKeyOutcome::Consumed
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            if state.cursor + 1 < menu.items.len() {
                state.cursor += 1;
            }
            MenuKeyOutcome::Consumed
        }
        (KeyCode::Enter, _) => {
            if let Some(item) = menu.items.get(state.cursor) {
                let cmd = item.command;
                state.close();
                MenuKeyOutcome::Run(cmd)
            } else {
                MenuKeyOutcome::Consumed
            }
        }
        _ => MenuKeyOutcome::Consumed,
    }
}

/// Compute the popover rect (anchored top-left under the ≡ affordance).
/// Falls back to the full area if it does not fit.
pub fn popup_rect(anchor: Rect, menu: &AppMenu) -> Rect {
    // Fixed width sized to fit "≡ rimeterm" heading + longest item ("Acknowledgement" + hint).
    let width: u16 = 34;
    let inner_rows = menu.items.len() as u16 + menu.items.iter().filter(|i| i.separator_before).count() as u16;
    let height: u16 = inner_rows + 2 /* borders */;
    let x = anchor.x.saturating_add(0);
    let y = anchor.y.saturating_add(1); // right under the status bar
    Rect {
        x,
        y,
        width: width.min(anchor.width.saturating_sub(x - anchor.x)),
        height: height.min(anchor.height.saturating_sub(y - anchor.y).max(1)),
    }
}

/// Render the popover. Caller MUST have cleared the underlying area if it
/// needs a solid background; we lay down a `Clear` widget ourselves.
pub fn render(area: Rect, buf: &mut Buffer, state: &MenuState, menu: &AppMenu) {
    if !state.open {
        return;
    }

    Clear.render(area, buf);
    let block = Block::default()
        .title(" ≡ rimeterm ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(menu.items.len() + 2);
    for (idx, item) in menu.items.iter().enumerate() {
        if item.separator_before {
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width.max(1) as usize),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        let icon = item.icon.unwrap_or(" ");
        let hint = item.key_hint.unwrap_or("");
        // Pad title so hint right-aligns without touching the title.
        let mut left = format!(" {} {}", icon, item.title);
        let right = format!(" {} ", hint);
        let width = inner.width as usize;
        if left.chars().count() + right.chars().count() < width {
            let pad = width - left.chars().count() - right.chars().count();
            left.push_str(&" ".repeat(pad));
        }
        let full = format!("{}{}", left, right);
        let mut style = Style::default();
        if idx == state.cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::styled(full, style));
    }
    Paragraph::new(lines)
        .alignment(Alignment::Left)
        .render(inner, buf);
}
