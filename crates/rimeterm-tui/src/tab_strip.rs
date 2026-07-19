//! Tab strip rendered above a [`TabGroup`]'s pane content.
//!
//! §19.10.6 visual conventions:
//! - Active tab drawn `│ NAME │` with reverse video.
//! - Inactive tabs plain text.
//! - Open groups (`shells`) append a `[+]` affordance.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use rimeterm_core::tabs::{MembersPolicy, TabGroup};

/// Draw the tab strip into `area` (typically one row above the group's rect).
///
/// `titles` MUST have the same length as `group.members()` and be aligned by
/// index. Caller is responsible for extracting the titles from its pane
/// registry so this module stays free of pane-registry deps.
pub fn render(area: Rect, buf: &mut Buffer, group: &TabGroup, titles: &[String]) {
    debug_assert_eq!(
        titles.len(),
        group.len(),
        "tab_strip: titles must match group members"
    );
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(group.len() * 2 + 2);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let active_style = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);

    spans.push(Span::styled(" ┤ ", dim));
    for (idx, title) in titles.iter().enumerate() {
        let is_active = idx == group.active_index();
        let label = format!(" {} ", title);
        if is_active {
            spans.push(Span::styled(label, active_style));
        } else {
            spans.push(Span::raw(label));
        }
        if idx + 1 < titles.len() {
            spans.push(Span::styled("│", dim));
        }
    }
    spans.push(Span::styled(" ├", dim));
    if matches!(group.policy(), MembersPolicy::Open { .. }) {
        spans.push(Span::styled(" [+]", dim));
    }

    Paragraph::new(Line::from(spans)).render(area, buf);
}
