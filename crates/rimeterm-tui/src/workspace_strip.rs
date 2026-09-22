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
/// `[+]` affordance rendered after the last tab: ` [+]`.
const NEW_TAB: &str = " [+]";
/// Separator between adjacent tab labels: ` │ `.
const SEPARATOR: &str = " │ ";

/// Double-click window for opening the rename editor. Matches
/// [`crate::pty_selection::MULTI_CLICK_MS`] / xterm's default.
const DOUBLE_CLICK_MS: u128 = 400;

/// Double-click streak detector that decides when a Down on a tab
/// opens the rename editor. `App` owns one permanently (streaks must
/// survive across clicks even when no editor is open); see
/// [`crate::pty_selection`] for the PTY-side equivalent.
#[derive(Debug, Clone)]
pub struct WorkspaceClickStreak {
    last_click: (u16, u16),
    last_click_at: std::time::Instant,
}

impl Default for WorkspaceClickStreak {
    fn default() -> Self {
        Self {
            last_click: (u16::MAX, u16::MAX),
            last_click_at: std::time::Instant::now()
                - std::time::Duration::from_millis(DOUBLE_CLICK_MS as u64 + 1),
        }
    }
}

impl WorkspaceClickStreak {
    /// Register a fresh `Down` at the given cell. Returns `true` when
    /// it lands on the same cell as the previous Down within
    /// [`DOUBLE_CLICK_MS`], i.e. the caller should treat it as the
    /// second half of a double-click.
    pub fn register(&mut self, col: u16, row: u16, now: std::time::Instant) -> bool {
        let same_cell = self.last_click == (col, row);
        let in_window = now.duration_since(self.last_click_at).as_millis() < DOUBLE_CLICK_MS;
        self.last_click = (col, row);
        self.last_click_at = now;
        same_cell && in_window
    }
}

/// In-progress rename of a workspace tab. `App` owns this; the strip
/// renders the edit pill while it is `Some`.
///
/// Keys while open (see `App::on_key`): typing appends, Backspace
/// deletes, Enter accepts (empty restores the auto title), Esc
/// cancels. A mouse Down anywhere else commits (browser inline-edit
/// blur behavior).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRename {
    /// Logical workspace index being renamed.
    pub index: usize,
    /// Draft text.
    pub buffer: String,
}

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
    rename: Option<&WorkspaceRename>,
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
        let is_renaming = rename.is_some_and(|r| r.index == idx);
        let label_style = if is_renaming {
            edit_pill_style()
        } else if is_active {
            active_style
        } else if is_hover_tab {
            tab_hover_style
        } else {
            Style::default()
        };
        let label = if is_renaming {
            let draft = rename.unwrap().buffer.clone();
            format!(" {draft}")
        } else {
            format!(" {title} ")
        };
        spans.push(Span::styled(label, label_style));
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

/// Reverse-video edit pill for the tab being renamed.
fn edit_pill_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Grid cell of the rename draft's block caret: walk the rendered
/// layout (prefix + each earlier tab's label/`×`/space + separator),
/// then 1 leading space + the draft width. `App::draw` uses this to
/// place the visible caret while the editor is open. Clamped to the
/// strip's last cell so an over-long draft can't push the caret off-row.
pub fn rename_caret_col(area: Rect, titles: &[String], rename: &WorkspaceRename) -> u16 {
    use unicode_width::UnicodeWidthStr;
    let end_x = area.x.saturating_add(area.width);
    let mut x = area.x.saturating_add(UnicodeWidthStr::width(PREFIX) as u16);
    for (idx, title) in titles.iter().enumerate() {
        if idx == rename.index {
            let caret = x.saturating_add(1 + UnicodeWidthStr::width(rename.buffer.as_str()) as u16);
            return caret.min(end_x.saturating_sub(1));
        }
        let label_w = UnicodeWidthStr::width(title.as_str()) as u16 + 2;
        // label + `×` + trailing space
        x = x.saturating_add(label_w + 2);
        if idx + 1 < titles.len() {
            x = x.saturating_add(UnicodeWidthStr::width(SEPARATOR) as u16);
        }
    }
    end_x.saturating_sub(1)
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
            render(
                f.area(),
                f.buffer_mut(),
                &titles(),
                1,
                WorkspaceHover::None,
                None,
            )
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
            render(
                f.area(),
                f.buffer_mut(),
                &titles(),
                0,
                WorkspaceHover::None,
                None,
            )
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

    /// Double-click streak: same cell within 400 ms promotes; a second
    /// click elsewhere or after the window resets.
    fn click_streak_promotes_on_double_click() {
        let mut streak = WorkspaceClickStreak::default();
        let t0 = std::time::Instant::now();
        assert!(!streak.register(30, 0, t0)); // first click
        assert!(streak.register(30, 0, t0 + std::time::Duration::from_millis(300)));
        // Different cell restarts the streak.
        assert!(!streak.register(35, 0, t0 + std::time::Duration::from_millis(500)));
        // Same cell but outside the window restarts.
        assert!(!streak.register(35, 0, t0 + std::time::Duration::from_secs(2)));
        assert!(!streak.register(35, 0, t0 + std::time::Duration::from_secs(3)));
    }

    /// Caret column walks the rendered layout: tab 0's caret sits after
    /// prefix + leading space + draft; tab 1's after tab 0's full
    /// label/close/pad + separator.
    #[test]
    fn rename_caret_col_walks_layout() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        };
        let t = titles();
        // Tab 0: prefix 21 + " " + draft "ab" → 24.
        let r0 = WorkspaceRename {
            index: 0,
            buffer: "ab".into(),
        };
        assert_eq!(rename_caret_col(area, &t, &r0), 24);
        // Tab 1: 21 + " alpha " (7) + "× " (2) + " │ " (3) + " " + "cd" → 36.
        let r1 = WorkspaceRename {
            index: 1,
            buffer: "cd".into(),
        };
        assert_eq!(rename_caret_col(area, &t, &r1), 36);
    }

    /// The renamed tab renders as a REVERSED pill with the draft text
    /// replacing the title.
    #[test]
    fn render_pill_replaces_active_title_with_draft() {
        let backend = TestBackend::new(80, 1);
        let mut term = Terminal::new(backend).unwrap();
        let rename = WorkspaceRename {
            index: 1,
            buffer: "draft".into(),
        };
        term.draw(|f| {
            render(
                f.area(),
                f.buffer_mut(),
                &titles(),
                1,
                WorkspaceHover::None,
                Some(&rename),
            )
        })
        .unwrap();
        let text: String = (0..80)
            .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(text.contains("draft"));
        assert!(!text.contains(" beta"));
        // Pill cells carry REVERSED; the replaced title's cells do not
        // (checked via the first draft cell).
        let pill_start = text.find(" draft").expect("pill present");
        let cell = &term.backend().buffer()[(pill_start as u16 + 1, 0)];
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
    }

    /// Over-long drafts clamp the caret to the strip's last cell.
    #[test]
    fn rename_caret_clamps_on_overflow() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 1,
        };
        let t = vec!["alpha".to_string()];
        let r = WorkspaceRename {
            index: 0,
            buffer: "x".repeat(20),
        };
        assert_eq!(rename_caret_col(area, &t, &r), 29);
    }
}
