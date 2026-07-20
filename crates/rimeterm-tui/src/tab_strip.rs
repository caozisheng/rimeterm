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

/// Deterministic layout of the tab strip for mouse hit-testing. Mirrors the
/// exact spans produced by [`render`] so a click on cell N ends up on the
/// right tab.
///
/// - Prefix ` ┤ ` = 3 cells.
/// - Each tab label = ` <title> ` = `unicode_width(title) + 2` cells.
/// - Inter-tab separator `│` = 1 cell.
/// - Suffix ` ├` = 2 cells.
/// - Optional ` [+]` (Open groups only) = 4 cells.
#[derive(Debug, Clone, Default)]
pub struct TabStripHits {
    /// The strip row itself.
    pub rect: Rect,
    /// (tab index, rect) — clicking inside `rect` activates that tab.
    pub tabs: Vec<(usize, Rect)>,
    /// `[+]` affordance rect (Open groups only). None when the group is Fixed.
    pub plus: Option<Rect>,
}

/// Compute the hit-rects the current [`render`] would produce for `area` +
/// `group` + `titles`. Cheap — O(members).
pub fn hit_rects(area: Rect, group: &TabGroup, titles: &[String]) -> TabStripHits {
    use unicode_width::UnicodeWidthStr;
    let y = area.y;
    let h = 1;
    let end_x = area.x.saturating_add(area.width);
    // Prefix " ┤ " — 3 columns.
    let mut x = area.x.saturating_add(3);
    let mut tabs = Vec::with_capacity(titles.len());
    for (idx, title) in titles.iter().enumerate() {
        let label_w = UnicodeWidthStr::width(title.as_str()) as u16 + 2;
        let w = label_w.min(end_x.saturating_sub(x));
        if w > 0 {
            tabs.push((idx, Rect { x, y, width: w, height: h }));
        }
        x = x.saturating_add(label_w);
        if idx + 1 < titles.len() {
            // Separator "│" — 1 column.
            x = x.saturating_add(1);
        }
    }
    // Suffix " ├" — 2 columns.
    x = x.saturating_add(2);
    let plus = if matches!(group.policy(), MembersPolicy::Open { .. }) {
        // " [+]" — 4 columns.
        let w = 4u16.min(end_x.saturating_sub(x));
        if w > 0 {
            Some(Rect { x, y, width: w, height: h })
        } else {
            None
        }
    } else {
        None
    };
    TabStripHits { rect: area, tabs, plus }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimeterm_core::pane::PaneId;
    use rimeterm_core::tabs::{PaneKind, TabGroup, TabGroupId};

    fn open_group(titles: &[&str]) -> (TabGroup, Vec<String>) {
        let ids: Vec<PaneId> = titles.iter().map(|_| PaneId::next()).collect();
        let g = TabGroup::new(
            TabGroupId::from_static("shells"),
            ids,
            MembersPolicy::Open { max: 8 },
            PaneKind::Shell,
        );
        (g, titles.iter().map(|s| (*s).to_string()).collect())
    }

    fn fixed_group(titles: &[&str]) -> (TabGroup, Vec<String>) {
        let ids: Vec<PaneId> = titles.iter().map(|_| PaneId::next()).collect();
        let g = TabGroup::new(
            TabGroupId::from_static("files"),
            ids,
            MembersPolicy::Fixed,
            PaneKind::Files,
        );
        (g, titles.iter().map(|s| (*s).to_string()).collect())
    }

    #[test]
    fn open_group_lays_out_tabs_plus_and_plus_affordance() {
        // area x=10 y=3 wide enough for everything.
        // Prefix at 10..13. First tab " shell-1 " = 9 cols at 13..22. Sep at 22.
        // Second tab " shell-2 " = 9 cols at 23..32. Suffix " ├" at 32..34.
        // Plus " [+]" at 34..38.
        let (g, titles) = open_group(&["shell-1", "shell-2"]);
        let h = hit_rects(
            Rect { x: 10, y: 3, width: 60, height: 1 },
            &g,
            &titles,
        );
        assert_eq!(h.rect.x, 10);
        assert_eq!(h.tabs.len(), 2);
        assert_eq!(h.tabs[0], (0, Rect { x: 13, y: 3, width: 9, height: 1 }));
        assert_eq!(h.tabs[1], (1, Rect { x: 23, y: 3, width: 9, height: 1 }));
        assert_eq!(h.plus, Some(Rect { x: 34, y: 3, width: 4, height: 1 }));
    }

    #[test]
    fn fixed_group_never_has_plus_affordance() {
        let (g, titles) = fixed_group(&["yazi", "gitui"]);
        let h = hit_rects(
            Rect { x: 0, y: 0, width: 60, height: 1 },
            &g,
            &titles,
        );
        assert!(h.plus.is_none());
        assert_eq!(h.tabs.len(), 2);
    }

    #[test]
    fn cjk_titles_use_display_width_not_byte_len() {
        let (g, titles) = open_group(&["构建"]); // 2 CJK chars = 4 columns
        let h = hit_rects(
            Rect { x: 0, y: 0, width: 60, height: 1 },
            &g,
            &titles,
        );
        // Prefix 0..3, label " 构建 " = 4 + 2 = 6 cols at 3..9.
        assert_eq!(h.tabs[0].1, Rect { x: 3, y: 0, width: 6, height: 1 });
    }

    #[test]
    fn narrow_area_clips_tab_rects() {
        let (g, titles) = open_group(&["long-title-here"]);
        let h = hit_rects(
            Rect { x: 0, y: 0, width: 8, height: 1 },
            &g,
            &titles,
        );
        // Prefix consumed 3 columns; only 5 left for the label.
        assert_eq!(h.tabs[0].1.width, 5);
    }
}
