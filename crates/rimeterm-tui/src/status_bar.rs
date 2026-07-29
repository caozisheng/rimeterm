//! Top status bar (row 0) rendering.
//!
//! Left slot:  `≡ rimeterm` — clickable main-menu opener.
//! Middle:     workspace label + branch (v0.1 stub).
//! Right slot: `shell: <name>` + a clickable `[×]` quit button.
//!
//! Both interactive glyphs (`≡` and `[×]`) get a hover style so the user
//! knows they can click them — terminals can't swap the OS cursor into a
//! pointing hand, so we compensate visually (same idea as the divider
//! hover paint, see `App::hovered_ui`).

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Paragraph, Widget};

/// Which interactive glyph in the status bar is under the mouse pointer
/// right now. `None` = the pointer is elsewhere. Callers must recompute
/// this on every `MouseEventKind::Moved` — the status bar hit rects
/// change with terminal width.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StatusBarHover {
    None,
    Menu,
    Quit,
}

/// Rects the mouse layer needs so `on_mouse` can route clicks back into
/// the right command (`app.menu.toggle`, `app.quit`). Populated by
/// [`render`] and cached on `App::last_status_bar_hits`.
#[derive(Debug, Clone, Default)]
pub struct StatusBarHits {
    /// `≡ rimeterm` label rect. `None` when the terminal is too narrow
    /// to fit even the label (rare).
    pub menu: Option<Rect>,
    /// `[×]` quit-button rect. `None` when the terminal is too narrow
    /// to fit it after workspace + shell (rare).
    pub quit: Option<Rect>,
}

/// Widths (cells) for the fixed side columns. Tuned so the labels
/// don't overflow on 80-column terminals — everything else flexes.
const MENU_WIDTH: u16 = 12; // " ≡ rimeterm"
const QUIT_WIDTH: u16 = 4; //  " [×]"
const SHELL_WIDTH: u16 = 18; // "shell: pwsh 7    "

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
    hover: StatusBarHover,
    key_hint: Option<&str>,
) -> StatusBarHits {
    // Five slots: menu | workspace (flex) | key-hint (fixed, opt) | shell | quit.
    // When the terminal shrinks below the sum of the fixed widths, `Layout` will
    // truncate the trailing slots — we surface `None` for anything that
    // came out zero-width so the mouse layer can't hit a phantom rect.
    // The key-hint column collapses to `Length(0)` when nothing to
    // show so the workspace flex reclaims every cell.
    //
    // Chip width = ` <hint> ` = unicode_width + 2. We use `chars().count()`
    // as a cheap proxy for display width; all the hints we ship
    // (ASCII + `·`) are single-cell so this stays tight.
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
            Constraint::Length(QUIT_WIDTH),
        ])
        .split(area);

    // ≡ menu opener. Bold by default (design §19.13.1: "hover 变粗").
    // Reverse on hover so it clearly reads as clickable even against
    // dark backgrounds where bold alone is hard to spot.
    let mut menu_style = Style::default();
    if matches!(hover, StatusBarHover::Menu) {
        menu_style = menu_style.add_modifier(Modifier::REVERSED);
    }
    Paragraph::new(" ≡ rimeterm")
        .style(menu_style)
        .render(cols[0], buf);

    Paragraph::new(format!("workspace: {}", workspace_label)).render(cols[1], buf);

    // Optional context-scoped key hint. Cyan so it reads as an
    // affordance without competing with the destructive-red quit
    // button. Rendered only when both a hint text and a non-zero
    // column exist (protects against `Layout` clamping the fixed
    // slot on a narrow terminal).
    if let Some(text) = key_hint {
        if cols[2].width > 0 {
            Paragraph::new(format!(" {} ", text))
                .style(Style::default().fg(Color::Cyan))
                .render(cols[2], buf);
        }
    }

    Paragraph::new(format!("shell: {}", shell_short))
        .style(Style::default().add_modifier(Modifier::DIM))
        .render(cols[3], buf);

    // Quit button. Red so it's unambiguous as a "close app" affordance;
    // reversed on hover for the same visibility reason as the menu.
    let mut quit_style = Style::default().fg(Color::LightRed);
    if matches!(hover, StatusBarHover::Quit) {
        quit_style = quit_style.add_modifier(Modifier::REVERSED);
    }
    Paragraph::new(" [×]")
        .style(quit_style)
        .render(cols[4], buf);

    StatusBarHits {
        menu: rect_if_nonzero(cols[0]),
        quit: rect_if_nonzero(cols[4]),
    }
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

    fn render_to_string(area: Rect, key_hint: Option<&str>) -> String {
        let mut buf = Buffer::empty(area);
        render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
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
        let s = render_to_string(Rect::new(0, 0, 80, 1), None);
        assert!(s.contains("workspace: myproj"));
        assert!(s.contains("shell: pwsh"));
        assert!(s.contains("[×]"));
        // No F9 chip.
        assert!(!s.contains("F9"));
    }

    #[test]
    fn key_hint_paints_between_workspace_and_shell() {
        let s = render_to_string(Rect::new(0, 0, 80, 1), Some("F9 menu"));
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
    fn hit_rects_align_with_menu_and_quit_slots() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let hits = render(
            area,
            &mut buf,
            "myproj",
            "pwsh",
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
            StatusBarHover::None,
            Some("F9 menu"),
        );
        // Either populated with a nonzero rect or None — never a
        // phantom zero-width rect.
        for r in [hits.menu, hits.quit].into_iter().flatten() {
            assert!(r.width > 0);
        }
    }
}
