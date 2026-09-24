//! Inline-scrollbar column preparation.
//!
//! rimeterm overlays right-edge scrollbars on top of content (the PTY
//! grid, List rows, Table columns) rather than reserving a dedicated
//! strip. Two ratatui behaviors break that overlay:
//!
//! 1. A wide glyph (CJK, emoji) in the column immediately left of the
//!    bar bleeds its right half across the bar column, and
//!    `ratatui_core::buffer::BufferDiff` advances past the covered cell
//!    while the wide glyph is present — so the bar segment for that row
//!    is never emitted and the thumb shows a hole whenever wide content
//!    scrolls past the second-to-last column.
//! 2. `Buffer::set_string` merges styles, so whatever colors /
//!    modifiers the content left under the bar column leak into the
//!    bar glyphs (e.g. a red filename turning a `║` segment red).
//!
//! [`prepare_scrollbar_column`] fixes both: narrow any wide glyph in
//! the neighbor column to a same-style space (the bar hides that half
//! of the cell anyway) and clear the covered column's symbol / fg /
//! modifiers while keeping its background so the bar lane blends with
//! the surrounding rows.

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;

/// Prepare the bar column of `area` for a scrollbar render.
///
/// `area` is the same rect passed to `Scrollbar::render` (ratatui
/// paints the bar in the last column of a `VerticalRight` area). Call
/// immediately before that render, and only when the scrollbar will
/// actually paint (a `content_length == 0` render draws nothing and
/// would leave a blanked column).
pub(crate) fn prepare_scrollbar_column(buf: &mut Buffer, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let sb_x = area.right().saturating_sub(1);
    for y in area.y..area.bottom() {
        // A wide glyph ending under the bar both covers the bar cell
        // physically and suppresses the diff for it. Narrow it to a
        // space, keeping its style so the row background stays uniform.
        if sb_x > 0 {
            let wide = buf.cell((sb_x - 1, y)).is_some_and(|c| c.cell_width() > 1);
            if wide && let Some(cell) = buf.cell_mut((sb_x - 1, y)) {
                cell.set_char(' ');
            }
        }
        // Content styling under the bar would merge into the bar
        // glyphs. Clear symbol / fg / modifiers; keep the background
        // so the bar lane doesn't punch a hole in filled rows.
        if let Some(cell) = buf.cell_mut((sb_x, y)) {
            let bg = cell.bg;
            cell.reset();
            cell.bg = bg;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn bar_col(x: u16, height: u16) -> Rect {
        Rect {
            x,
            y: 0,
            width: 1,
            height,
        }
    }

    #[test]
    fn multi_column_area_targets_its_last_column() {
        // git_pane / stock_pane pass the full inner rect (the same one
        // handed to Scrollbar::render); the bar lives in its last column.
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 5));
        buf[(10, 1)].set_char('汉');
        buf[(11, 1)].set_char('z').set_fg(Color::Red);
        buf[(3, 2)].set_char('q');

        prepare_scrollbar_column(&mut buf, Rect::new(0, 0, 12, 5));

        assert_eq!(buf[(10, 1)].symbol(), " ");
        assert_eq!(buf[(11, 1)].symbol(), " ");
        assert_eq!(buf[(11, 1)].fg, Color::Reset);
        // Interior cells untouched.
        assert_eq!(buf[(3, 2)].symbol(), "q");
    }

    #[test]
    fn wide_neighbor_is_narrowed_with_style_kept() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        buf[(8, 1)].set_char('汉').set_fg(Color::Green);
        buf[(8, 2)].set_char('x');

        prepare_scrollbar_column(&mut buf, bar_col(9, 5));

        assert_eq!(buf[(8, 1)].symbol(), " ");
        assert_eq!(buf[(8, 1)].fg, Color::Green);
        // Narrow neighbors are left alone.
        assert_eq!(buf[(8, 2)].symbol(), "x");
    }

    #[test]
    fn covered_column_style_leak_is_cleared_but_bg_kept() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        buf[(9, 3)]
            .set_char('z')
            .set_fg(Color::Red)
            .set_bg(Color::Blue);

        prepare_scrollbar_column(&mut buf, bar_col(9, 5));

        assert_eq!(buf[(9, 3)].symbol(), " ");
        assert_eq!(buf[(9, 3)].fg, Color::Reset);
        assert_eq!(buf[(9, 3)].bg, Color::Blue);
    }

    #[test]
    fn bar_at_column_zero_skips_neighbor_narrowing() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        buf[(0, 0)].set_char('汉');

        prepare_scrollbar_column(&mut buf, bar_col(0, 5));

        // Nothing to the left; the covered cell itself is just cleared.
        assert_eq!(buf[(0, 0)].symbol(), " ");
    }

    /// The core regression: with the neighbor narrowed, a thumb update
    /// on the (previously covered) row reaches the terminal diff.
    #[test]
    fn thumb_update_on_wide_row_reaches_the_terminal() {
        let area = Rect::new(0, 0, 10, 5);
        let frame = |thumb_row: u16| {
            let mut b = Buffer::empty(area);
            b[(8, 2)].set_char('汉');
            prepare_scrollbar_column(&mut b, bar_col(9, 5));
            for y in 0..5 {
                b[(9, y)].set_char('║');
            }
            b[(9, thumb_row)].set_char('█');
            b
        };

        let prev = frame(2);
        let next = frame(3);

        let updates: Vec<(u16, u16)> = prev.diff(&next).iter().map(|&(x, y, _)| (x, y)).collect();
        assert!(
            updates.contains(&(9, 2)),
            "thumb leaving the wide row must be re-emitted, got {updates:?}"
        );
        assert!(updates.contains(&(9, 3)));
    }

    /// Documents why the narrowing exists: with the wide glyph intact,
    /// `BufferDiff` skips the covered bar cell, so a thumb moving off
    /// that row never repaints it on the terminal — the visible "broken
    /// scrollbar" while CJK output scrolls. If this test ever fails,
    /// ratatui changed the skip and `prepare_scrollbar_column` may be
    /// simplifiable.
    #[test]
    fn un_narrowed_wide_glyph_suppresses_the_covered_update() {
        let area = Rect::new(0, 0, 10, 5);
        let mut prev = Buffer::empty(area);
        prev[(8, 2)].set_char('汉');
        for y in 0..5 {
            prev[(9, y)].set_char('║');
        }
        prev[(9, 2)].set_char('█');

        let mut next = prev.clone();
        next[(9, 2)].set_char('║');

        let updates: Vec<(u16, u16)> = prev.diff(&next).iter().map(|&(x, y, _)| (x, y)).collect();
        assert!(
            !updates.contains(&(9, 2)),
            "expected the covered cell to be skipped by the diff, got {updates:?}"
        );
    }
}
