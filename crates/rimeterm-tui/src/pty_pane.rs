//! Single-session PTY pane.
//!
//! Wraps [`rimeterm_pty::Session`] and blits its vt100 grid into a ratatui
//! [`Buffer`]. v0.1 renders cell-by-cell using [`vt100::Screen`] APIs — good
//! enough for correctness in the M0 skeleton; a later pass batches contiguous
//! runs of same-style cells for speed.

use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_pty::{Decision, ResizeThrottle, Session};

use std::time::Instant;

pub struct PtyPane {
    id: PaneId,
    title: String,
    session: Session,
    last_area: Rect,
    /// PTY resize throttler (§19.12.6). See [`rimeterm_pty::ResizeThrottle`].
    resize: ResizeThrottle,
}

impl PtyPane {
    pub fn new(session: Session, title: impl Into<String>) -> Self {
        Self {
            id: PaneId::next(),
            title: title.into(),
            session,
            last_area: Rect::default(),
            resize: ResizeThrottle::platform(),
        }
    }

    /// Kill the child process (used at shutdown).
    pub fn kill(&self) {
        self.session.kill();
    }

    /// Access the underlying session (needed for the pty read-loop wakeup).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Immediately push any pending resize to the PTY. Called from the app
    /// main loop on mouse-up so the final drag size lands exactly, and on
    /// each frame so the debounce window can expire between events.
    pub fn tick_resize(&mut self, now: Instant) {
        match self.resize.poll(now) {
            Decision::Apply { cols, rows } => {
                let _ = self.session.resize(cols.max(2), rows.max(1));
            }
            Decision::Idle | Decision::Wait => {}
        }
    }

    /// Force-flush any pending resize (bypasses the debounce window). Call
    /// on mouse-up / drag-end so the final size is exact regardless of when
    /// the window would have expired.
    pub fn flush_resize_now(&mut self) {
        if let Some((cols, rows)) = self.resize.flush_now() {
            let _ = self.session.resize(cols.max(2), rows.max(1));
        }
    }
}

impl PaneProvider for PtyPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn set_title(&mut self, title: String) -> bool {
        self.title = title;
        true
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: true,
            holds_foreground_work: true,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &PaneRenderCtx) -> RenderOutcome {
        // Focus visuals: focused = bright cyan + bold + `▶ …` title marker
        // so it also reads in monochrome / low-contrast terminals; unfocused
        // = dim grey. `LightCyan` alone was hard to see on dark themes.
        let marker = if ctx.focused { "▶ " } else { "  " };
        let title = format!(" {}🐚 {} ", marker, self.title);
        let border_style = if ctx.focused {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);

        // Request a resize through the throttler (§19.12.6). Actual PTY
        // resize happens either when `tick_resize` sees the idle window
        // elapse, or when the app forces a flush on mouse-up.
        //
        // Special case: the very first render bypasses the throttle so the
        // child sees the correct size before its splash frame — Ink apps
        // (oh-my-pi, opencode) render their layout once at spawn and don't
        // reflow well from an 80x24 start.
        if inner != self.last_area {
            if inner.width >= 2 && inner.height >= 1 {
                let first_render = self.last_area == Rect::default();
                self.resize
                    .request(inner.width.max(2), inner.height.max(1), Instant::now());
                if first_render {
                    self.flush_resize_now();
                }
            }
            self.last_area = inner;
        }
        // Cheap poll — no-op when nothing is pending.
        self.tick_resize(Instant::now());

        // Blit vt100 grid cell-by-cell.
        self.session.with_grid(|parser| {
            let screen = parser.screen();
            for row in 0..inner.height {
                for col in 0..inner.width {
                    let cell_x = inner.x + col;
                    let cell_y = inner.y + row;
                    let Some(cell) = screen.cell(row, col) else {
                        continue;
                    };
                    let target = &mut buf[(cell_x, cell_y)];
                    let ch = cell.contents();
                    if ch.is_empty() {
                        target.set_char(' ');
                    } else {
                        // For wide chars we currently write the first codepoint;
                        // a proper impl would set the second cell to `empty()`.
                        let first = ch.chars().next().unwrap_or(' ');
                        target.set_char(first);
                    }
                    let mut style = Style::default();
                    style = apply_vt100_color(style, cell.fgcolor(), true);
                    style = apply_vt100_color(style, cell.bgcolor(), false);
                    if cell.bold() {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if cell.italic() {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if cell.underline() {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    if cell.inverse() {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    target.set_style(style);
                }
            }
        });

        RenderOutcome {
            request_redraw: false,
        }
    }

    fn flush_pending_resize(&mut self) {
        self.flush_resize_now();
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if let Some(bytes) = encode_key(key) {
            let _ = self.session.write(&bytes);
            true
        } else {
            false
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent, outer_rect: Rect) -> bool {
        // Border occupies 1 cell on every side; clicks on the border are
        // not forwarded to the child (users are targeting the pane frame,
        // typically to grab focus).
        let inner = inner_rect(outer_rect);
        if !point_in_rect(ev.column, ev.row, inner) {
            return false;
        }
        // xterm SGR mouse expects **1-based, inside-content** coordinates.
        let x = ev.column - inner.x + 1;
        let y = ev.row - inner.y + 1;
        if let Some(bytes) = encode_sgr_mouse(ev.kind, ev.modifiers, x, y) {
            let _ = self.session.write(&bytes);
            true
        } else {
            false
        }
    }
}

fn apply_vt100_color(style: Style, color: vt100::Color, foreground: bool) -> Style {
    let c = match color {
        vt100::Color::Default => return style,
        vt100::Color::Idx(i) => match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            other => Color::Indexed(other),
        },
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    };
    if foreground {
        style.fg(c)
    } else {
        style.bg(c)
    }
}

/// Inset the outer pane rect by 1 cell on every side to match the block
/// border we draw in `render`. `saturating_sub` guards against absurdly
/// small rects (e.g. 0×0 during teardown) so the returned rect is always
/// well-formed.
pub(crate) fn inner_rect(outer: Rect) -> Rect {
    let inset_w = outer.width.saturating_sub(2);
    let inset_h = outer.height.saturating_sub(2);
    Rect {
        x: outer.x.saturating_add(1),
        y: outer.y.saturating_add(1),
        width: inset_w,
        height: inset_h,
    }
}

/// True when `(x, y)` lies inside `r` (inclusive left/top, exclusive
/// right/bottom, matching everywhere else in the app).
pub(crate) fn point_in_rect(x: u16, y: u16, r: Rect) -> bool {
    x >= r.x
        && x < r.x.saturating_add(r.width)
        && y >= r.y
        && y < r.y.saturating_add(r.height)
}

/// Encode a crossterm `MouseEvent` as an xterm SGR mouse sequence
/// (`ESC[<button;X;YM` for press/motion, `ESC[<button;X;Ym` for release).
/// `x` / `y` are **1-based pane-local content coordinates**.
///
/// SGR button byte layout (xterm ctlseqs):
///   bits 0..1 = button (0=left, 1=middle, 2=right, 3=release/motion)
///   bit  2    = shift    (+4)
///   bit  3    = meta/alt (+8)
///   bit  4    = ctrl     (+16)
///   bit  5    = motion   (+32)
///   bit  6    = wheel    (+64)      (buttons 64=up, 65=down)
///
/// Returns `None` for events we don't forward (e.g. `Moved` without a
/// held button — most apps ignore those and floods add up).
pub(crate) fn encode_sgr_mouse(
    kind: MouseEventKind,
    mods: KeyModifiers,
    x: u16,
    y: u16,
) -> Option<Vec<u8>> {
    let (mut button, is_release) = match kind {
        MouseEventKind::Down(b) => (button_code(b)?, false),
        MouseEventKind::Up(b) => (button_code(b)?, true),
        MouseEventKind::Drag(b) => (button_code(b)? | 0b0010_0000, false), // +motion
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Moved => return None,
    };
    if mods.contains(KeyModifiers::SHIFT) {
        button |= 0b0000_0100;
    }
    if mods.contains(KeyModifiers::ALT) {
        button |= 0b0000_1000;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        button |= 0b0001_0000;
    }
    let final_char = if is_release { 'm' } else { 'M' };
    Some(format!("\x1b[<{};{};{}{}", button, x, y, final_char).into_bytes())
}

fn button_code(b: MouseButton) -> Option<u8> {
    Some(match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    })
}

/// Translate a `crossterm::KeyEvent` into raw bytes for the pty.
///
/// v0.1 covers the common cases: printable chars, Enter, Tab, Backspace,
/// arrows, Ctrl+letter (excluding Ctrl+C which the app menu intercepts).
fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out = Vec::with_capacity(4);
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Z / Ctrl+@..
                if let 'a'..='z' = c.to_ascii_lowercase() {
                    out.push((c.to_ascii_uppercase() as u8) - b'@');
                } else {
                    return None;
                }
            } else {
                if alt {
                    out.push(0x1b);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    fn ev(kind: MouseEventKind, x: u16, y: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: mods }
    }

    #[test]
    fn inner_rect_insets_by_1_on_each_side() {
        let r = inner_rect(Rect { x: 5, y: 4, width: 20, height: 10 });
        assert_eq!(r, Rect { x: 6, y: 5, width: 18, height: 8 });
    }

    #[test]
    fn inner_rect_saturates_on_tiny_outer() {
        let r = inner_rect(Rect { x: 0, y: 0, width: 1, height: 1 });
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
    }

    #[test]
    fn sgr_left_press_at_1_1_encodes_correctly() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            1,
            1,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<0;1;1M");
    }

    #[test]
    fn sgr_right_release_encodes_lowercase_m() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Up(MouseButton::Right),
            KeyModifiers::NONE,
            10,
            20,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<2;10;20m");
    }

    #[test]
    fn sgr_drag_sets_motion_bit() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::NONE,
            5,
            7,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<32;5;7M"); // 0 (left) | 32 (motion)
    }

    #[test]
    fn sgr_scroll_wheel_uses_64_65() {
        let up = encode_sgr_mouse(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 4)
            .unwrap();
        let down = encode_sgr_mouse(MouseEventKind::ScrollDown, KeyModifiers::NONE, 3, 4)
            .unwrap();
        assert_eq!(up, b"\x1b[<64;3;4M");
        assert_eq!(down, b"\x1b[<65;3;4M");
    }

    #[test]
    fn sgr_shift_ctrl_alt_modifiers_add_bits() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
            1,
            1,
        )
        .unwrap();
        // 0 (left) | 4 (shift) | 8 (alt) | 16 (ctrl) = 28
        assert_eq!(bytes, b"\x1b[<28;1;1M");
    }

    #[test]
    fn sgr_moved_without_button_is_dropped() {
        assert!(
            encode_sgr_mouse(MouseEventKind::Moved, KeyModifiers::NONE, 1, 1).is_none()
        );
    }

    #[test]
    fn point_in_rect_edges() {
        let r = Rect { x: 5, y: 5, width: 3, height: 2 };
        assert!(point_in_rect(5, 5, r));
        assert!(point_in_rect(7, 6, r));
        assert!(!point_in_rect(8, 6, r)); // width exclusive
        assert!(!point_in_rect(7, 7, r)); // height exclusive
        assert!(!point_in_rect(4, 5, r));
    }

    #[test]
    fn on_mouse_ignores_clicks_on_border() {
        // ev.column/row on the border cells should NOT produce forwarded bytes.
        // We can't easily construct a live PtyPane in a unit test, but the
        // border-check lives in inner_rect + point_in_rect which the above
        // tests cover. This test just documents the intent.
        let outer = Rect { x: 0, y: 0, width: 10, height: 5 };
        let inner = inner_rect(outer);
        assert!(!point_in_rect(0, 0, inner)); // top-left border cell
        assert!(!point_in_rect(9, 0, inner)); // top-right border cell
        assert!(!point_in_rect(0, 4, inner)); // bottom-left border cell
        assert!(point_in_rect(1, 1, inner)); // first content cell
    }
}
