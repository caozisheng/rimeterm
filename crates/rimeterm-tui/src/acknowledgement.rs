//! Modal overlay that renders the bundled `ACKNOWLEDGEMENTS.md` file.
//!
//! The app-menu "Acknowledgement" entry used to be a stub that only
//! logged + set a hint. This module makes it actually show the file's
//! contents so the credits are one keypress away.
//!
//! Design:
//! - Content is `include_str!`-baked at compile time. Released binaries
//!   don't need to locate `ACKNOWLEDGEMENTS.md` on disk (the file may
//!   be installed to `usr/share/doc/rimeterm/` on Linux or nowhere at
//!   all on portable extracts) — we always have the exact version that
//!   was built.
//! - Rendering is line-based, not a full markdown parse: headings get a
//!   bold+cyan style, `-` bullets get a leading marker, links stay as
//!   raw source. That's good enough for a credits file — bringing in
//!   the full `rimeterm-markdown` renderer would be overkill for what
//!   is effectively a single scrollable text pane.
//! - Same overlay ergonomics as `SettingsState`: Esc/Enter/q closes,
//!   `j`/`k`/arrows scroll one line, `PageUp`/`PageDown` scroll a page,
//!   `g`/`G` jump to top/bottom.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

/// Bundled `ACKNOWLEDGEMENTS.md`. Baked at compile time so the overlay
/// works in every distribution (portable archive, .deb, .msi, dev
/// checkout) without a runtime path search.
const ACK_TEXT: &str = include_str!("../../../ACKNOWLEDGEMENTS.md");

#[derive(Debug, Default)]
pub struct AckOverlayState {
    pub open: bool,
    /// Zero-based index of the first visible line. Clamped to
    /// `line_count.saturating_sub(1)` on every scroll so it can never
    /// point past the last line.
    scroll: usize,
    /// Cached count of pre-styled lines. Populated on `open()` and
    /// reused across frames so scroll math doesn't re-parse the file.
    line_count: usize,
}

impl AckOverlayState {
    pub fn open(&mut self) {
        self.open = true;
        self.scroll = 0;
        // Cheap: split on '\n'. `Line::styled` per row happens in
        // `render`, keyed off `self.scroll`, so we don't cache the
        // Ratatui `Line` objects here (they'd own owned strings for
        // dozens of lines we might never scroll to).
        self.line_count = ACK_TEXT.lines().count();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Returns `true` if the key was handled (caller should stop
    /// propagating). Called BEFORE any other overlay in the key path
    /// so the ack modal is truly modal while it's up.
    pub fn handle_key(&mut self, key: KeyEvent, page_rows: u16) -> bool {
        if !self.open {
            return false;
        }
        let page = page_rows.max(1) as usize;
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.close();
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::PageUp => self.scroll_by(-(page as isize)),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page as isize),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.scroll = self.line_count.saturating_sub(1);
            }
            _ => {}
        }
        true
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.line_count.saturating_sub(1) as isize;
        let next = (self.scroll as isize + delta).clamp(0, max);
        self.scroll = next as usize;
    }

    /// Popup rect. Same 90%/90% shape as the Settings modal for a
    /// consistent modal feel.
    pub fn popup_rect(&self, area: Rect) -> Rect {
        let width = (area.width * 9 / 10).clamp(48, 120);
        let height = (area.height * 9 / 10).clamp(12, 48);
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.open {
            return;
        }
        let popup = self.popup_rect(area);
        Clear.render(popup, buf);

        let block = Block::default()
            .title(" Acknowledgement ")
            .title_bottom(" [j/k/PgUp/PgDn/g/G] scroll · [Esc/Enter/q] close ")
            .borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);

        // Skip the leading H1 ("# Acknowledgements") — the overlay
        // title bar already says the same thing.
        let visible: Vec<Line> = ACK_TEXT
            .lines()
            .skip(self.scroll)
            .take(inner.height as usize)
            .map(style_line)
            .collect();
        Paragraph::new(visible).render(inner, buf);
    }
}

/// Very small line-based styler. Not a markdown parser — good enough
/// for a credits file whose whole grammar is `#` / `##` headings, `-`
/// bullets, and `[label](url)` links.
fn style_line(raw: &str) -> Line<'_> {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix("## ") {
        return Line::styled(
            format!(" {rest}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        return Line::styled(
            format!(" {rest}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return Line::from(vec![
            Span::styled("  • ", Style::default().fg(Color::DarkGray)),
            Span::raw(rest.to_string()),
        ]);
    }
    if raw.is_empty() {
        return Line::from(Span::raw(""));
    }
    Line::styled(format!(" {raw}"), Style::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn open_and_esc_close() {
        let mut s = AckOverlayState::default();
        assert!(!s.open);
        s.open();
        assert!(s.open);
        assert!(s.line_count > 0, "ACKNOWLEDGEMENTS.md should be non-empty");
        let handled = s.handle_key(key(KeyCode::Esc), 20);
        assert!(handled);
        assert!(!s.open);
    }

    #[test]
    fn scroll_is_clamped_to_line_count() {
        let mut s = AckOverlayState::default();
        s.open();
        // Way past the end.
        s.handle_key(key(KeyCode::End), 20);
        assert_eq!(s.scroll, s.line_count.saturating_sub(1));
        // And way past the start.
        s.handle_key(key(KeyCode::Home), 20);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn keys_are_ignored_when_closed() {
        let mut s = AckOverlayState::default();
        assert!(!s.handle_key(key(KeyCode::Down), 20));
        assert!(!s.open);
    }

    #[test]
    fn heading_gets_bold_style() {
        let line = style_line("## TUI");
        // ratatui's `Line::styled` stores the style at line level,
        // not on each span — check both places so this test survives
        // an internal ratatui refactor either way.
        let line_bold = line.style.add_modifier.contains(Modifier::BOLD);
        let span_bold = line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            line_bold || span_bold,
            "## headings should render bold (line.style={:?}, spans={:?})",
            line.style,
            line.spans.iter().map(|s| s.style).collect::<Vec<_>>()
        );
    }
}
