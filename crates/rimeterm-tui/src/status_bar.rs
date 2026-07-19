//! Top status bar (row 0) rendering.
//!
//! Left slot: `≡ rimeterm` (main menu access affordance).
//! Middle:   workspace label + branch (v0.1 stub).
//! Right:    active agent + clock (v0.1 stub).
//!
//! v0.1 does not query git or the agent state; it renders placeholders so the
//! layout math is exercised. Real hooks arrive when the corresponding crates
//! come online.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Paragraph, Widget};

/// Draw the status bar into `area`. The caller reserves a 1-row rect.
pub fn render(area: Rect, buf: &mut Buffer, workspace_label: &str, shell_short: &str) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(14), // ≡ rimeterm
            Constraint::Min(0),
            Constraint::Length(20),
        ])
        .split(area);

    Paragraph::new("≡ rimeterm")
        .style(Style::default().add_modifier(Modifier::BOLD))
        .render(cols[0], buf);

    Paragraph::new(format!("workspace: {}", workspace_label)).render(cols[1], buf);

    Paragraph::new(format!("shell: {}", shell_short))
        .style(Style::default().add_modifier(Modifier::DIM))
        .render(cols[2], buf);
}
