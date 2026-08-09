//! Top status bar (row 0) rendering.
//!
//! The right side orders `shell: <name>`, the layout segmented toggle, and
//! the quit button. Interactive controls use reversed hover/selection styles.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Paragraph, Widget};
use rimeterm_config::memory_state::WorkspaceLayoutMode;

/// Which interactive glyph in the status bar is under the mouse pointer
/// right now. `None` = the pointer is elsewhere. Callers must recompute
/// this on every `MouseEventKind::Moved` — the status bar hit rects
/// change with terminal width.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StatusBarHover {
    None,
    Menu,
    Landscape,
    Vertical,
    Quit,
}

/// Rects used by the app mouse router for status-bar controls.
#[derive(Debug, Clone, Default)]
pub struct StatusBarHits {
    pub menu: Option<Rect>,
    pub landscape: Option<Rect>,
    pub vertical: Option<Rect>,
    pub quit: Option<Rect>,
}

const MENU_WIDTH: u16 = 12;
const QUIT_WIDTH: u16 = 4;
const SHELL_WIDTH: u16 = 18;
const LAYOUT_LANDSCAPE_WIDTH: u16 = 11;
const LAYOUT_VERTICAL_WIDTH: u16 = 10;

/// Draw the status bar into `area`. The caller reserves a 1-row rect.
///
/// `key_hint` is an optional right-aligned chip painted just before the
/// shell column — used to surface context-scoped keybinds (e.g. the
/// viewer overlay advertises `F9 menu` because its own modal chrome
/// doesn't have room for the affordance). `None` = no chip; the
/// workspace label takes the full flex slot.
///
/// Returns the hit rects for the clickable affordances. See
/// [`StatusBarHits`].
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    workspace_label: &str,
    shell_short: &str,
    layout_mode: WorkspaceLayoutMode,
    hover: StatusBarHover,
    key_hint: Option<&str>,
) -> StatusBarHits {
    let hint_col_width = key_hint
        .map(|s| (s.chars().count() as u16).saturating_add(2))
        .unwrap_or(0);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(MENU_WIDTH),
            Constraint::Min(0),
            Constraint::Length(hint_col_width),
            Constraint::Length(SHELL_WIDTH),
            Constraint::Length(LAYOUT_LANDSCAPE_WIDTH),
            Constraint::Length(LAYOUT_VERTICAL_WIDTH),
            Constraint::Length(QUIT_WIDTH),
        ])
        .split(area);

    let menu_style = if matches!(hover, StatusBarHover::Menu) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Paragraph::new(" ≡ rimeterm")
        .style(menu_style)
        .render(cols[0], buf);
    Paragraph::new(format!("workspace: {workspace_label}")).render(cols[1], buf);
    if let Some(text) = key_hint
        && cols[2].width > 0
    {
        Paragraph::new(format!(" {text} "))
            .style(Style::default().fg(Color::Cyan))
            .render(cols[2], buf);
    }
    Paragraph::new(format!("shell: {shell_short}"))
        .style(Style::default().add_modifier(Modifier::DIM))
        .render(cols[3], buf);

    render_layout_segment(
        cols[4],
        buf,
        " LANDSCAPE ",
        layout_mode == WorkspaceLayoutMode::Landscape,
        matches!(hover, StatusBarHover::Landscape),
    );
    render_layout_segment(
        cols[5],
        buf,
        " VERTICAL ",
        layout_mode == WorkspaceLayoutMode::Vertical,
        matches!(hover, StatusBarHover::Vertical),
    );

    let quit_style = if matches!(hover, StatusBarHover::Quit) {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(Color::LightRed)
    };
    Paragraph::new(" [×]")
        .style(quit_style)
        .render(cols[6], buf);

    StatusBarHits {
        menu: rect_if_nonzero(cols[0]),
        landscape: rect_if_nonzero(cols[4]),
        vertical: rect_if_nonzero(cols[5]),
        quit: rect_if_nonzero(cols[6]),
    }
}

fn render_layout_segment(area: Rect, buf: &mut Buffer, label: &str, selected: bool, hovered: bool) {
    let mut style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    if hovered && !selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Paragraph::new(label).style(style).render(area, buf);
}

fn rect_if_nonzero(r: Rect) -> Option<Rect> {
    if r.width == 0 || r.height == 0 {
        None
    } else {
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn render_to_string(
        area: Rect,
        key_hint: Option<&str>,
        layout_mode: WorkspaceLayoutMode,
    ) -> String {
        let mut buf = Buffer::empty(area);
        render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
            layout_mode,
            StatusBarHover::None,
            key_hint,
        );
        let mut out = String::new();
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, area.y)) {
                out.push_str(cell.symbol());
            }
        }
        out
    }

    #[test]
    fn key_hint_none_gives_workspace_the_full_flex_slot() {
        let s = render_to_string(
            Rect::new(0, 0, 100, 1),
            None,
            WorkspaceLayoutMode::Landscape,
        );
        assert!(s.contains("workspace: myproj"));
        assert!(s.contains("shell: pwsh"));
        assert!(s.contains("[×]"));
        // No F9 chip.
        assert!(!s.contains("F9"));
    }

    #[test]
    fn key_hint_paints_between_workspace_and_shell() {
        let s = render_to_string(
            Rect::new(0, 0, 100, 1),
            Some("F9 menu"),
            WorkspaceLayoutMode::Landscape,
        );
        assert!(s.contains("workspace: myproj"));
        assert!(s.contains("F9 menu"));
        assert!(s.contains("shell: pwsh"));
        // Order: workspace before F9 before shell.
        let w = s.find("workspace").unwrap();
        let h = s.find("F9 menu").unwrap();
        let sh = s.find("shell:").unwrap();
        assert!(w < h && h < sh, "column order broken: {s:?}");
    }

    #[test]
    fn vertical_segment_is_selected_in_vertical_mode() {
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        let hits = render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
            WorkspaceLayoutMode::Vertical,
            StatusBarHover::None,
            None,
        );
        let vertical = hits.vertical.expect("vertical hit");

        assert!(
            buf[(vertical.x + 1, vertical.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );
    }
    #[test]
    fn layout_toggle_renders_between_shell_and_quit() {
        let s = render_to_string(Rect::new(0, 0, 100, 1), None, WorkspaceLayoutMode::Vertical);

        let shell = s.find("shell:").expect("shell label");
        let landscape = s.find("LANDSCAPE").expect("landscape segment");
        let vertical = s.find("VERTICAL").expect("vertical segment");
        let quit = s.find("[×]").expect("quit button");
        assert!(
            shell < landscape && landscape < vertical && vertical < quit,
            "{s:?}"
        );
    }

    #[test]
    fn hit_rects_align_with_menu_and_quit_slots() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let hits = render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
            WorkspaceLayoutMode::Landscape,
            StatusBarHover::None,
            Some("F9 menu"),
        );
        // Menu rect must be the leftmost slot; quit must be the rightmost.
        let menu = hits.menu.expect("menu hit populated on wide terminal");
        let quit = hits.quit.expect("quit hit populated on wide terminal");
        assert_eq!(menu.x, 0);
        assert_eq!(menu.width, MENU_WIDTH);
        assert_eq!(quit.x + quit.width, area.width);
        assert_eq!(quit.width, QUIT_WIDTH);
    }

    #[test]
    fn layout_segments_have_distinct_hit_rects() {
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        let hits = render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
            WorkspaceLayoutMode::Landscape,
            StatusBarHover::None,
            None,
        );

        let landscape = hits.landscape.expect("landscape hit");
        let vertical = hits.vertical.expect("vertical hit");
        assert_eq!(landscape.x + landscape.width, vertical.x);
    }

    #[test]
    fn narrow_terminal_still_paints_menu_and_quit_or_reports_none() {
        // At 20 cells the shell column can't fully fit, but menu + quit
        // must still land somewhere sensible (or resolve to None if the
        // slot was clamped to zero).
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        let hits = render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
            WorkspaceLayoutMode::Landscape,
            StatusBarHover::None,
            Some("F9 menu"),
        );
        // Either populated with a nonzero rect or None — never a
        // phantom zero-width rect.
        for r in [hits.menu, hits.landscape, hits.vertical, hits.quit]
            .into_iter()
            .flatten()
        {
            assert!(r.width > 0);
        }
    }
}
