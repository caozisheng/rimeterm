//! Single-session PTY pane.
//!
//! Wraps [`rimeterm_pty::Session`] and blits its vt100 grid into a ratatui
//! [`Buffer`]. v0.1 renders cell-by-cell using [`vt100::Screen`] APIs — good
//! enough for correctness in the M0 skeleton; a later pass batches contiguous
//! runs of same-style cells for speed.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
