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

/// Widths (cells) for the two fixed side columns. Tuned so the labels
/// don't overflow on 80-column terminals — everything else flexes.
const MENU_WIDTH: u16 = 12; // " ≡ rimeterm"
const QUIT_WIDTH: u16 = 4; //  " [×]"
const SHELL_WIDTH: u16 = 18; // "shell: pwsh 7    "

/// Draw the status bar into `area`. The caller reserves a 1-row rect.
///
/// Returns the hit rects for the clickable affordances. See
/// [`StatusBarHits`].
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    workspace_label: &str,
    shell_short: &str,
    hover: StatusBarHover,
) -> StatusBarHits {
    // Four slots: menu | workspace (flex) | shell | quit. When the
    // terminal shrinks below the sum of the fixed widths, `Layout` will
    // truncate the trailing slots — we surface `None` for anything that
    // came out zero-width so the mouse layer can't hit a phantom rect.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(MENU_WIDTH),
            Constraint::Min(0),
            Constraint::Length(SHELL_WIDTH),
            Constraint::Length(QUIT_WIDTH),
        ])
        .split(area);

    // ≡ menu opener. Bold by default (design §19.13.1: "hover 变粗").
    // Reverse on hover so it clearly reads as clickable even against
    // dark backgrounds where bold alone is hard to spot.
    let mut menu_style = Style::default().add_modifier(Modifier::BOLD);
    if matches!(hover, StatusBarHover::Menu) {
        menu_style = menu_style.add_modifier(Modifier::REVERSED);
    }
    Paragraph::new(" ≡ rimeterm")
        .style(menu_style)
        .render(cols[0], buf);

    Paragraph::new(format!("workspace: {}", workspace_label)).render(cols[1], buf);

    Paragraph::new(format!("shell: {}", shell_short))
        .style(Style::default().add_modifier(Modifier::DIM))
        .render(cols[2], buf);

    // Quit button. Red so it's unambiguous as a "close app" affordance;
    // reversed on hover for the same visibility reason as the menu.
    let mut quit_style = Style::default()
        .fg(Color::LightRed)
        .add_modifier(Modifier::BOLD);
    if matches!(hover, StatusBarHover::Quit) {
        quit_style = quit_style.add_modifier(Modifier::REVERSED);
    }
    Paragraph::new(" [×]")
        .style(quit_style)
        .render(cols[3], buf);

    StatusBarHits {
        menu: rect_if_nonzero(cols[0]),
        quit: rect_if_nonzero(cols[3]),
    }
}

fn rect_if_nonzero(r: Rect) -> Option<Rect> {
    if r.width == 0 || r.height == 0 {
        None
    } else {
        Some(r)
    }
}
