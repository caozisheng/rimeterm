//! Workspace tab strip rendered at the top of the terminal when
//! workspace-tab management is enabled
//! (`rimeterm_config::workspaces_state`).
//!
//! Visual: ` rimeterm workspace: name1 | name2 [+]` — active title
//! UNDERLINED (per the settled design; unlike the in-group tab strips
//! which use REVERSED, the workspace strip is a global chrome row and
//! underline keeps it visually distinct from the focused group's strip).
//! Each tab carries a hover-able `×` close affordance using the same
//! 2-cell geometry convention as [`crate::tab_strip`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Which workspace-strip affordance the pointer is over right now.
/// Drives hover styling in [`render`] the same way
/// [`crate::tab_strip::TabStripHover`] does for group strips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceHover {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    New,
}

/// A hit-test result for the workspace strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceHit {
    /// Click activates workspace `usize`.
    Tab(usize),
    /// Click closes workspace `usize` (last tab routes through the exit
    /// dialog, see `App::close_workspace`).
    Close(usize),
    /// Click duplicates the active workspace (`[+]` affordance).
    New,
}

/// Prefix cell width: ` rimeterm workspace: `.
const PREFIX: &str = " rimeterm workspace: ";
/// Separator between adjacent tab labels: ` │ `.
const SEPARATOR: &str = " │ ";
/// `[+]` affordance rendered after the last tab: ` [+]`.
const NEW_TAB: &str = " [+]";

/// Draw the workspace strip into `area` (one row).
///
/// `titles` and `active` come from `App`'s workspace bookkeeping
/// (`ws_titles` / `active_ws`). Only the active title is underlined;
/// inactive titles restyle on hover.
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    titles: &[String],
    active: usize,
    hover: WorkspaceHover,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let active_style = Style::default().add_modifier(Modifier::UNDERLINED);
    let tab_hover_style = Style::default().add_modifier(Modifier::BOLD);
    let close_hover_style = Style::default().fg(Color::LightRed);
    let new_hover_style = Style::default();

    let mut spans: Vec<Span<'_>> = Vec::with_capacity(titles.len() * 4 + 3);
    spans.push(Span::styled(PREFIX, dim));
    for (idx, title) in titles.iter().enumerate() {
        let is_active = idx == active;
        let is_hover_tab = matches!(hover, WorkspaceHover::Tab(i) if i == idx);
        let label_style = if is_active {
            active_style
        } else if is_hover_tab {
            tab_hover_style
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {title} "), label_style));
        let is_hover_close = matches!(hover, WorkspaceHover::Close(i) if i == idx);
        let close_style = if is_hover_close {
            close_hover_style
        } else {
            dim
        };
        spans.push(Span::styled("×", close_style));
        spans.push(Span::raw(" "));
        if idx + 1 < titles.len() {
            spans.push(Span::styled(SEPARATOR, dim));
        }
    }
    let new_style = if matches!(hover, WorkspaceHover::New) {
        new_hover_style
    } else {
        dim
    };
    spans.push(Span::styled(NEW_TAB, new_style));

    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Deterministic layout of [`render`] for mouse hit-testing. Mirrors the
/// exact spans painted so a click on cell N hits the affordance the user
/// sees. Geometry (columns):
///
/// - Prefix ` rimeterm workspace: ` = 21 cells (not clickable).
/// - Each tab label ` <title> ` = `unicode_width(title) + 2`.
/// - Close `× ` per tab = 2 cells; only the `×` cell itself is a hit
///   rect (a click on the padding space is a no-op, matching
///   [`crate::tab_strip::hit_rects`]).
/// - Separator ` │ ` = 3 cells between tabs (not clickable).
/// - ` [+]` = 4 cells at the end; all 4 are the New hit rect.
///
/// Rects are clipped to `area` and empty rects dropped, so a strip
/// overflowing a narrow terminal simply loses trailing affordances.
pub fn hit_rects(area: Rect, titles: &[String]) -> Vec<(Rect, WorkspaceHit)> {
    use unicode_width::UnicodeWidthStr;

    let end_x = area.x.saturating_add(area.width);
    let y = area.y;
    let h = 1u16;
    let mut hits = Vec::with_capacity(titles.len() * 2 + 1);
    let mut push = |x: u16, w: u16, hit: WorkspaceHit| {
        let w = w.min(end_x.saturating_sub(x));
        if w > 0 {
            hits.push((
                Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                hit,
            ));
        }
    };

    let prefix_w = UnicodeWidthStr::width(PREFIX) as u16;
    let mut x = area.x.saturating_add(prefix_w);
    for (idx, title) in titles.iter().enumerate() {
        let label_w = UnicodeWidthStr::width(title.as_str()) as u16 + 2;
        push(x, label_w, WorkspaceHit::Tab(idx));
        x = x.saturating_add(label_w);
        // `×` cell is the close hit; the trailing space is dead.
        push(x, 1, WorkspaceHit::Close(idx));
        x = x.saturating_add(2);
        if idx + 1 < titles.len() {
            x = x.saturating_add(UnicodeWidthStr::width(SEPARATOR) as u16);
        }
    }
    let new_w = UnicodeWidthStr::width(NEW_TAB) as u16;
    push(x, new_w, WorkspaceHit::New);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn titles() -> Vec<String> {
        vec!["alpha".into(), "beta".into()]
    }

    /// Prefix ends at x=21 (` rimeterm workspace: ` = 21 cols).
    #[test]
    fn hit_rects_map_tabs_closes_and_new() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        };
        let hits = hit_rects(area, &titles());
        let kinds: Vec<WorkspaceHit> = hits.iter().map(|(_, h)| *h).collect();
        assert_eq!(
            kinds,
            vec![
                WorkspaceHit::Tab(0),
                WorkspaceHit::Close(0),
                WorkspaceHit::Tab(1),
                WorkspaceHit::Close(1),
                WorkspaceHit::New,
            ]
        );
        // " alpha " = 7 cols at 21..28, close × at 28, padding space 29.
        assert_eq!(
            hits[0].0,
            Rect {
                x: 21,
                y: 0,
                width: 7,
                height: 1
            }
        );
        assert_eq!(
            hits[1].0,
            Rect {
                x: 28,
                y: 0,
                width: 1,
                height: 1
            }
        );
        // separator " │ " = 3 cols (30..33) not clickable; " beta " at 33..39.
        assert_eq!(
            hits[2].0,
            Rect {
                x: 33,
                y: 0,
                width: 6,
                height: 1
            }
        );
        assert_eq!(
            hits[3].0,
            Rect {
                x: 39,
                y: 0,
                width: 1,
                height: 1
            }
        );
        // " [+]" = 4 cols at 41..45.
        assert_eq!(
            hits[4].0,
            Rect {
                x: 41,
                y: 0,
                width: 4,
                height: 1
            }
        );
    }

    /// Prefix, separator, and dead padding never produce hit rects —
    /// only the tab labels, `×` cells, and `[+]` are interactive.
    #[test]
    fn prefix_and_separators_are_not_clickable() {
        let area = Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 1,
        };
        for (rect, _) in hit_rects(area, &titles()) {
            assert!(rect.x >= 21, "hit inside prefix at x={}", rect.x);
            assert_ne!(rect.x, 30, "hit on separator");
        }
    }

    /// Narrow terminal: overflowing affordances are clipped away rather
    /// than hit-testing phantom cells.
    #[test]
    fn overflow_clips_trailing_hits() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 26,
            height: 1,
        };
        let hits = hit_rects(area, &titles());
        // Only tab 0 (21..28 clipped to 21..26) and its × (28 → clipped
        // out, end_x=26) partially survive.
        let kinds: Vec<WorkspaceHit> = hits.iter().map(|(_, h)| *h).collect();
        assert_eq!(kinds, vec![WorkspaceHit::Tab(0)]);
        assert_eq!(hits[0].0.width, 5);
    }

    #[test]
    fn render_underlines_only_active_title() {
        let backend = TestBackend::new(80, 1);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            render(f.area(), f.buffer_mut(), &titles(), 1, WorkspaceHover::None);
        })
        .unwrap();

        let buf = term.backend().buffer().clone();
        // Locate each title's first cell by scanning for 'a' / 'b' after
        // the prefix.
        let find = |ch: char| -> Style {
            for x in 21..40u16 {
                let cell = &buf[(x, 0)];
                if cell.symbol() == ch.to_string() {
                    return cell.style();
                }
            }
            panic!("{ch} not found");
        };
        let alpha_style = find('a');
        let beta_style = find('b');
        assert!(!alpha_style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(beta_style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn render_contains_all_titles_and_plus() {
        let backend = TestBackend::new(80, 1);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            render(f.area(), f.buffer_mut(), &titles(), 0, WorkspaceHover::None);
        })
        .unwrap();
        let text: String = (0..80)
            .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(text.contains("rimeterm workspace:"));
        assert!(text.contains("alpha"));
        assert!(text.contains("beta"));
        assert!(text.contains("[+]"));
        assert!(text.contains("│"));
    }
}
