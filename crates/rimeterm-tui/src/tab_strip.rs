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
/// index. `closable` is a per-tab override consulted only for Open groups: a
/// tab in an Open group with `closable[idx] == false` renders **without**
/// its `×` affordance so users can't dismiss it with the mouse. Fixed
/// groups ignore `closable` entirely (they never render `×`). Pass an empty
/// slice to accept the default "every tab in an Open group is closable".
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    group: &TabGroup,
    titles: &[String],
    closable: &[bool],
) {
    debug_assert_eq!(
        titles.len(),
        group.len(),
        "tab_strip: titles must match group members"
    );
    debug_assert!(
        closable.is_empty() || closable.len() == group.len(),
        "tab_strip: closable must be empty or match group members"
    );
    let group_closable = matches!(group.policy(), MembersPolicy::Open { .. });
    let is_closable = |idx: usize| -> bool {
        if !group_closable {
            return false;
        }
        closable.get(idx).copied().unwrap_or(true)
    };
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(group.len() * 3 + 3);
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
        // Close affordance is per-tab now (§19.10.1 addendum: bottom is
        // pinned inside the shells group). Skipping the `×` also skips
        // the trailing space so the tab strip stays visually tight
        // around pinned tabs.
        if is_closable(idx) {
            spans.push(Span::styled("×", dim));
            spans.push(Span::raw(" "));
        }
        if idx + 1 < titles.len() {
            spans.push(Span::styled("│", dim));
        }
    }
    spans.push(Span::styled(" ├", dim));
    if group_closable {
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
/// - Optional `× ` per closable tab (Open groups) = 2 cells.
/// - Inter-tab separator `│` = 1 cell.
/// - Suffix ` ├` = 2 cells.
/// - Optional ` [+]` (Open groups only) = 4 cells.
#[derive(Debug, Clone, Default)]
pub struct TabStripHits {
    /// The strip row itself.
    pub rect: Rect,
    /// (tab index, rect) — clicking inside `rect` activates that tab.
    pub tabs: Vec<(usize, Rect)>,
    /// (tab index, rect) — clicking inside `rect` closes that tab. Only
    /// populated for closable tabs in Open groups.
    pub closes: Vec<(usize, Rect)>,
    /// `[+]` affordance rect (Open groups only). None when the group is Fixed.
    pub plus: Option<Rect>,
}

/// Compute the hit-rects the current [`render`] would produce for `area` +
/// `group` + `titles` + `closable`. Cheap — O(members).
///
/// `closable` follows the same semantics as [`render`]: empty means "every
/// tab in an Open group is closable"; otherwise `closable[idx]` decides
/// whether the close rect exists for that tab. Skipped close rects also
/// skip the 2-cell `× ` suffix from the geometry, matching what `render`
/// paints so hit-tests never point at a phantom `×` cell.
pub fn hit_rects(
    area: Rect,
    group: &TabGroup,
    titles: &[String],
    closable: &[bool],
) -> TabStripHits {
    use unicode_width::UnicodeWidthStr;
    debug_assert!(
        closable.is_empty() || closable.len() == group.len(),
        "tab_strip: closable must be empty or match group members"
    );
    let y = area.y;
    let h = 1;
    let end_x = area.x.saturating_add(area.width);
    let group_closable = matches!(group.policy(), MembersPolicy::Open { .. });
    let is_closable = |idx: usize| -> bool {
        if !group_closable {
            return false;
        }
        closable.get(idx).copied().unwrap_or(true)
    };
    // Prefix " ┤ " — 3 columns.
    let mut x = area.x.saturating_add(3);
    let mut tabs = Vec::with_capacity(titles.len());
    let mut closes: Vec<(usize, Rect)> =
        Vec::with_capacity(if group_closable { titles.len() } else { 0 });
    for (idx, title) in titles.iter().enumerate() {
        let label_w = UnicodeWidthStr::width(title.as_str()) as u16 + 2;
        let w = label_w.min(end_x.saturating_sub(x));
        if w > 0 {
            tabs.push((
                idx,
                Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
            ));
        }
        x = x.saturating_add(label_w);
        if is_closable(idx) {
            // "×" (1 col) then " " (1 col). Treat the `×` cell alone as the
            // hit rect so a click on the trailing space still counts as
            // "clicked between tabs" (no-op) rather than a close by
            // accident.
            if end_x.saturating_sub(x) >= 1 {
                closes.push((
                    idx,
                    Rect {
                        x,
                        y,
                        width: 1,
                        height: h,
                    },
                ));
            }
            x = x.saturating_add(2); // "×" + " "
        }
        if idx + 1 < titles.len() {
            // Separator "│" — 1 column.
            x = x.saturating_add(1);
        }
    }
    // Suffix " ├" — 2 columns.
    x = x.saturating_add(2);
    let plus = if group_closable {
        // " [+]" — 4 columns.
        let w = 4u16.min(end_x.saturating_sub(x));
        if w > 0 {
            Some(Rect {
                x,
                y,
                width: w,
                height: h,
            })
        } else {
            None
        }
    } else {
        None
    };
    TabStripHits {
        rect: area,
        tabs,
        closes,
        plus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimeterm_core::PaneKind;
    use rimeterm_core::pane::PaneId;
    use rimeterm_core::tabs::TabGroupId;

    fn open_group(members: &[&str]) -> (TabGroup, Vec<String>) {
        let ids: Vec<PaneId> = (1..=members.len()).map(|n| PaneId(n as u64)).collect();
        let g = TabGroup::new(
            TabGroupId::from_static("shells"),
            ids,
            MembersPolicy::Open { max: 16 },
            PaneKind::Shell,
        );
        (g, members.iter().map(|s| (*s).to_owned()).collect())
    }

    fn fixed_group(members: &[&str]) -> (TabGroup, Vec<String>) {
        let ids: Vec<PaneId> = (1..=members.len()).map(|n| PaneId(n as u64)).collect();
        let g = TabGroup::new(
            TabGroupId::from_static("files"),
            ids,
            MembersPolicy::Fixed,
            PaneKind::Files,
        );
        (g, members.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn open_group_lays_out_tabs_closes_and_plus_affordance() {
        //  Prefix " ┤ " at 10..13.
        //  Tab 0 " shell-1 " = 9 cols at 13..22, close "×" at 22, gap " " at 23,
        //  separator "│" at 24.
        //  Tab 1 " shell-2 " = 9 cols at 25..34, close "×" at 34, gap " " at 35.
        //  Suffix " ├" at 36..38. Plus " [+]" at 38..42.
        let (g, titles) = open_group(&["shell-1", "shell-2"]);
        let h = hit_rects(
            Rect {
                x: 10,
                y: 3,
                width: 60,
                height: 1,
            },
            &g,
            &titles,
            &[],
        );
        assert_eq!(h.rect.x, 10);
        assert_eq!(h.tabs.len(), 2);
        assert_eq!(
            h.tabs[0],
            (
                0,
                Rect {
                    x: 13,
                    y: 3,
                    width: 9,
                    height: 1
                }
            )
        );
        assert_eq!(
            h.tabs[1],
            (
                1,
                Rect {
                    x: 25,
                    y: 3,
                    width: 9,
                    height: 1
                }
            )
        );
        assert_eq!(h.closes.len(), 2);
        assert_eq!(
            h.closes[0],
            (
                0,
                Rect {
                    x: 22,
                    y: 3,
                    width: 1,
                    height: 1
                }
            )
        );
        assert_eq!(
            h.closes[1],
            (
                1,
                Rect {
                    x: 34,
                    y: 3,
                    width: 1,
                    height: 1
                }
            )
        );
        assert_eq!(
            h.plus,
            Some(Rect {
                x: 38,
                y: 3,
                width: 4,
                height: 1
            })
        );
    }

    #[test]
    fn fixed_group_has_no_plus_or_close_affordance() {
        let (g, titles) = fixed_group(&["yazi", "gitui"]);
        let h = hit_rects(
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 1,
            },
            &g,
            &titles,
            &[],
        );
        assert!(h.plus.is_none());
        assert!(
            h.closes.is_empty(),
            "fixed groups never expose close affordances"
        );
        assert_eq!(h.tabs.len(), 2);
    }

    #[test]
    fn cjk_titles_use_display_width_not_byte_len() {
        let (g, titles) = open_group(&["构建"]); // 2 CJK chars = 4 columns
        let h = hit_rects(
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 1,
            },
            &g,
            &titles,
            &[],
        );
        // Prefix 0..3, label " 构建 " = 4 + 2 = 6 cols at 3..9.
        assert_eq!(
            h.tabs[0].1,
            Rect {
                x: 3,
                y: 0,
                width: 6,
                height: 1
            }
        );
        // Close "×" at column 9 (right after the label, before the trailing " ").
        assert_eq!(
            h.closes[0].1,
            Rect {
                x: 9,
                y: 0,
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn narrow_area_clips_tab_rects() {
        let (g, titles) = open_group(&["long-title-here"]);
        let h = hit_rects(
            Rect {
                x: 0,
                y: 0,
                width: 8,
                height: 1,
            },
            &g,
            &titles,
            &[],
        );
        // Prefix consumed 3 columns; only 5 left for the label.
        assert_eq!(h.tabs[0].1.width, 5);
    }

    #[test]
    fn pinned_first_tab_hides_close_and_tightens_layout() {
        // §19.10.1 addendum: bottom is the pinned first tab of the
        // shells group. Its `×` disappears, AND the following tab must
        // slide left by 2 cells (the "× " suffix we skip).
        let (g, titles) = open_group(&["bottom", "shell-1"]);
        // closable[0] = false pins the first tab; closable[1] = true
        // keeps the second tab's `×`.
        let closable = vec![false, true];
        let h = hit_rects(
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 1,
            },
            &g,
            &titles,
            &closable,
        );
        // Prefix " ┤ " = 3, label " bottom " = 8, no "× ", separator "│" = 1.
        assert_eq!(
            h.tabs[0].1,
            Rect {
                x: 3,
                y: 0,
                width: 8,
                height: 1
            }
        );
        // shell-1 label starts at 3 + 8 + 1 = 12.
        assert_eq!(
            h.tabs[1].1,
            Rect {
                x: 12,
                y: 0,
                width: 9,
                height: 1
            }
        );
        // Only one close rect — bottom lost its `×`.
        assert_eq!(h.closes.len(), 1);
        assert_eq!(h.closes[0].0, 1);
    }

    #[test]
    fn all_pinned_leaves_no_closes_but_keeps_plus() {
        // Degenerate: pinning every tab still leaves the group Open,
        // so `[+]` stays reachable. No close rects should appear.
        let (g, titles) = open_group(&["bottom"]);
        let h = hit_rects(
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 1,
            },
            &g,
            &titles,
            &[false],
        );
        assert!(h.closes.is_empty());
        assert!(h.plus.is_some());
    }
}
