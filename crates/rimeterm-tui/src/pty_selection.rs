//! Local mouse selection + system clipboard for [`PtyPane`].
//!
//! C22.6: rimeterm can own the mouse for text selection when the child
//! program hasn't asked for xterm mouse reports. Selection state lives
//! per-pane so each PTY tab keeps its own highlight.
//!
//! The heavy lifting is here so `pty_pane.rs` stays focused on the
//! grid-to-buffer paint path and PTY plumbing.
//!
//! ## Coordinate system
//!
//! All rows/cols are **inner-content, 0-based**: `(0, 0)` is the top-left
//! visible cell after subtracting the pane border. Scrollback is out of
//! scope for v1 — selection only spans the currently-visible viewport.
//!
//! ## Life cycle
//!
//! 1. `Down(Left)` → [`SelectionState::begin`] anchors at `(row, col)`.
//!    A repeat click at the same spot within
//!    [`MULTI_CLICK_MS`] promotes to [`Granularity::Word`] then
//!    [`Granularity::Line`] (xterm behaviour).
//! 2. `Drag(Left)` → [`SelectionState::extend`] moves the cursor.
//! 3. `Up(Left)` → [`SelectionState::commit`] freezes the range and
//!    returns the extracted text so the caller can push it to arboard.
//! 4. A fresh `Down(Left)` on empty space or a new anchor resets the
//!    state.
//!
//! `Shift+Left` inside `Down` extends the existing selection instead of
//! starting a new one (Alacritty / xterm convention).

use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;

/// Maximum gap between clicks for double-/triple-click detection. 400 ms
/// matches xterm's default and Windows' `GetDoubleClickTime` median.
const MULTI_CLICK_MS: u128 = 400;

/// Word-boundary character class. A "word" is a run of non-whitespace
/// non-punctuation characters — this lets double-click grab
/// `foo.rs`, `crates/rimeterm-tui`, or `192.168.0.1` in one shot,
/// matching what tmux / Windows Terminal / iTerm2 all do.
fn is_word_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '"' | '\''
        )
}

/// Selection granularity, promoted on double- / triple-click.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Granularity {
    #[default]
    Char,
    Word,
    Line,
}

/// Per-pane selection state. Empty (`None` anchor) means "no active
/// selection"; a non-empty state means the highlight is either being
/// dragged (still tracking mouse) or frozen after `commit`.
#[derive(Clone, Debug, Default)]
pub struct SelectionState {
    /// The stationary end of the selection (grabbed on `Down`).
    /// `None` = no selection at all.
    anchor: Option<Cell>,
    /// The moving end of the selection. Equal to `anchor` on a fresh
    /// `Down` with no drag yet.
    cursor: Cell,
    /// How to interpret the (anchor, cursor) range at extract time.
    mode: Granularity,
    /// When `commit` runs, we flip this true so the highlight stays
    /// visible until the next `Down` — matches xterm.
    frozen: bool,
    /// Timestamp + cell of the most recent `Down`, used to detect
    /// double/triple-click.
    last_click: Option<(Instant, Cell)>,
    /// Click streak: 1 = char, 2 = word, 3 = line, wrapping back to 1.
    click_streak: u8,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub row: u16,
    pub col: u16,
}

impl SelectionState {
    /// Start a fresh selection. `now` is passed in (rather than
    /// captured internally) so unit tests can drive multi-click without
    /// racing wall-clock.
    ///
    /// If the click lands on the same cell within [`MULTI_CLICK_MS`] of
    /// the previous `begin`, the granularity promotes: 1 → 2 (word) →
    /// 3 (line) → back to 1 (char).
    pub fn begin(&mut self, at: Cell, now: Instant) {
        let streak = match self.last_click {
            Some((t, cell)) if cell == at && now.duration_since(t).as_millis() < MULTI_CLICK_MS => {
                (self.click_streak % 3) + 1
            }
            _ => 1,
        };
        self.click_streak = streak;
        self.last_click = Some((now, at));
        self.mode = match streak {
            2 => Granularity::Word,
            3 => Granularity::Line,
            _ => Granularity::Char,
        };
        self.anchor = Some(at);
        self.cursor = at;
        self.frozen = false;
    }

    /// Extend the moving end during a drag. Silent no-op if there's no
    /// active anchor.
    pub fn extend(&mut self, to: Cell) {
        if self.anchor.is_some() {
            self.cursor = to;
            self.frozen = false;
        }
    }

    /// Extend from an existing anchor without changing granularity —
    /// used by `Shift+Left` to grow the previous selection instead of
    /// starting a new one.
    pub fn shift_extend(&mut self, to: Cell) {
        if self.anchor.is_none() {
            // Nothing to extend — treat as a fresh char-mode click.
            self.anchor = Some(to);
            self.click_streak = 1;
            self.mode = Granularity::Char;
        }
        self.cursor = to;
        self.frozen = false;
    }

    /// Freeze the highlight after `Up`. The highlight persists until the
    /// next `Down` (or `clear`), so users can see what they just copied.
    pub fn commit(&mut self) {
        if self.anchor.is_some() {
            self.frozen = true;
        }
    }

    /// Wipe the selection entirely (called on cell-change fallthrough,
    /// resize, or explicit clear via `Esc`).
    pub fn clear(&mut self) {
        self.anchor = None;
        self.cursor = Cell { row: 0, col: 0 };
        self.frozen = false;
        self.mode = Granularity::Char;
        // Deliberately keep `last_click` — a user can single-click,
        // release, then click again quickly and still get word mode.
    }

    /// True if there's any active or frozen highlight to render / copy.
    pub fn is_active(&self) -> bool {
        self.anchor.is_some()
    }

    /// True when the highlight has been committed (mouse-up seen).
    /// Renderer paints the same reverse-video overlay either way; this
    /// is here so tests can assert the state machine.
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Return the currently-active granularity (mainly for tests).
    pub fn granularity(&self) -> Granularity {
        self.mode
    }

    /// Return `(top_left, bottom_right)` in char-mode ordering (row
    /// major). Callers use it to check "is `(r, c)` inside the
    /// highlight?" while painting. Returns `None` for an empty
    /// selection.
    pub fn char_range(&self) -> Option<(Cell, Cell)> {
        let anchor = self.anchor?;
        let (start, end) = if (anchor.row, anchor.col) <= (self.cursor.row, self.cursor.col) {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some((start, end))
    }

    /// True when `(row, col)` is inside the currently-highlighted range,
    /// respecting granularity + grid width so word/line highlights fill
    /// past the raw cursor position.
    pub fn contains(&self, row: u16, col: u16, cols: u16) -> bool {
        let Some((start, end)) = self.char_range() else {
            return false;
        };
        match self.mode {
            Granularity::Line => row >= start.row && row <= end.row,
            Granularity::Char | Granularity::Word => {
                // Char / word share the same "flowed rectangle"
                // painter: first row runs from start.col to end of
                // line; middle rows are full; last row runs 0..=end.col.
                if row < start.row || row > end.row {
                    return false;
                }
                if start.row == end.row {
                    col >= start.col && col <= end.col
                } else if row == start.row {
                    col >= start.col && col < cols
                } else if row == end.row {
                    col <= end.col
                } else {
                    col < cols
                }
            }
        }
    }
}

/// Extract the plaintext content of the current selection out of
/// `term`'s visible grid.
///
/// Returns `None` for an empty or trivially-empty selection. The
/// caller pushes the string to arboard.
///
/// ### Wrap handling
///
/// Alacritty marks a cell with [`Flags::WRAPLINE`] when its row
/// continued into the next row *because the parser wrapped*, not
/// because the user pressed Enter. We use it to *not* insert a `\n`
/// between such rows so pasting long shell lines round-trips
/// correctly. Real newlines land at row boundaries whose LAST
/// non-blank cell lacks the flag.
///
/// ### Wide chars
///
/// A wide char occupies two grid columns; the trailing cell has
/// [`Flags::WIDE_CHAR_SPACER`]. We skip spacers so we never emit the
/// grapheme twice.
pub fn extract_text<T>(term: &Term<T>, sel: &SelectionState) -> Option<String> {
    let (start, end) = sel.char_range()?;
    let cols = term.grid().columns() as u16;
    let mut out = String::new();

    for row in start.row..=end.row {
        let (row_start, row_end) = row_bounds(sel, row, start, end, cols);
        let line_wrapped = row_is_wrapped(term, row, cols);
        let mut row_str = String::new();
        let line: i32 = row.into();

        for col in row_start..=row_end {
            let point = Point::new(Line(line), Column(col as usize));
            let cell = &term.grid()[point];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            row_str.push(if cell.c == '\0' { ' ' } else { cell.c });
        }

        // Trim trailing spaces on non-wrapped rows so a selection of
        // three short shell prompts doesn't come out padded to full
        // width. Wrapped rows keep their spaces — they're mid-token.
        if !line_wrapped {
            let trimmed_end = row_str.trim_end();
            row_str.truncate(trimmed_end.len());
        }
        out.push_str(&row_str);
        if row < end.row && !line_wrapped {
            out.push('\n');
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Per-row `(start_col, end_col)` clamped to `cols - 1`, respecting
/// granularity.
fn row_bounds(sel: &SelectionState, row: u16, start: Cell, end: Cell, cols: u16) -> (u16, u16) {
    let max_col = cols.saturating_sub(1);
    match sel.mode {
        Granularity::Line => (0, max_col),
        Granularity::Char | Granularity::Word => {
            let s = if row == start.row { start.col } else { 0 };
            let e = if row == end.row { end.col } else { max_col };
            (s.min(max_col), e.min(max_col))
        }
    }
}

/// True when `row` ends on a cell whose `WRAPLINE` flag is set. That's
/// alacritty's way of marking "row continues into row+1 mid-token".
fn row_is_wrapped<T>(term: &Term<T>, row: u16, cols: u16) -> bool {
    if cols == 0 {
        return false;
    }
    let line: i32 = row.into();
    let point = Point::new(Line(line), Column((cols - 1) as usize));
    term.grid()[point].flags.contains(Flags::WRAPLINE)
}

/// Expand a char-mode range into a word-mode one by growing both ends
/// outward across `is_word_char` runs. Called after `begin` when
/// `granularity == Word`. Mutates `sel` in place.
pub fn snap_to_word<T>(sel: &mut SelectionState, term: &Term<T>) {
    let Some(anchor) = sel.anchor else {
        return;
    };
    let cols = term.grid().columns() as u16;

    // Snap the anchor leftward.
    let left = word_boundary_left(term, anchor.row, anchor.col, cols);
    // Snap the cursor rightward (may equal anchor on a fresh double-click).
    let right = word_boundary_right(term, anchor.row, anchor.col, cols);
    sel.anchor = Some(Cell {
        row: anchor.row,
        col: left,
    });
    sel.cursor = Cell {
        row: anchor.row,
        col: right,
    };
}

fn word_boundary_left<T>(term: &Term<T>, row: u16, col: u16, cols: u16) -> u16 {
    let line: i32 = row.into();
    let mut c = col;
    let start_char = char_at(term, line, c, cols);
    if !start_char.map_or(false, is_word_char) {
        return c;
    }
    while c > 0 {
        let next = c - 1;
        match char_at(term, line, next, cols) {
            Some(ch) if is_word_char(ch) => c = next,
            _ => break,
        }
    }
    c
}

fn word_boundary_right<T>(term: &Term<T>, row: u16, col: u16, cols: u16) -> u16 {
    let line: i32 = row.into();
    let mut c = col;
    let start_char = char_at(term, line, c, cols);
    if !start_char.map_or(false, is_word_char) {
        return c;
    }
    let last = cols.saturating_sub(1);
    while c < last {
        let next = c + 1;
        match char_at(term, line, next, cols) {
            Some(ch) if is_word_char(ch) => c = next,
            _ => break,
        }
    }
    c
}

fn char_at<T>(term: &Term<T>, line: i32, col: u16, cols: u16) -> Option<char> {
    if col >= cols {
        return None;
    }
    let point = Point::new(Line(line), Column(col as usize));
    let ch = term.grid()[point].c;
    if ch == '\0' { None } else { Some(ch) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cell(row: u16, col: u16) -> Cell {
        Cell { row, col }
    }

    #[test]
    fn begin_default_is_char_mode() {
        let mut s = SelectionState::default();
        s.begin(cell(0, 0), Instant::now());
        assert_eq!(s.granularity(), Granularity::Char);
        assert!(s.is_active());
        assert!(!s.is_frozen());
    }

    #[test]
    fn second_click_same_cell_promotes_to_word() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(2, 5), t0);
        s.begin(cell(2, 5), t0 + Duration::from_millis(100));
        assert_eq!(s.granularity(), Granularity::Word);
    }

    #[test]
    fn third_click_promotes_to_line() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(2, 5), t0);
        s.begin(cell(2, 5), t0 + Duration::from_millis(100));
        s.begin(cell(2, 5), t0 + Duration::from_millis(200));
        assert_eq!(s.granularity(), Granularity::Line);
    }

    #[test]
    fn fourth_click_wraps_back_to_char() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(2, 5), t0);
        s.begin(cell(2, 5), t0 + Duration::from_millis(100));
        s.begin(cell(2, 5), t0 + Duration::from_millis(200));
        s.begin(cell(2, 5), t0 + Duration::from_millis(300));
        assert_eq!(s.granularity(), Granularity::Char);
    }

    #[test]
    fn slow_second_click_stays_char() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(2, 5), t0);
        s.begin(cell(2, 5), t0 + Duration::from_millis(500));
        assert_eq!(s.granularity(), Granularity::Char);
    }

    #[test]
    fn different_cell_resets_streak() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(2, 5), t0);
        s.begin(cell(2, 6), t0 + Duration::from_millis(100));
        assert_eq!(s.granularity(), Granularity::Char);
    }

    #[test]
    fn extend_moves_cursor_but_not_anchor() {
        let mut s = SelectionState::default();
        s.begin(cell(0, 0), Instant::now());
        s.extend(cell(3, 10));
        let (start, end) = s.char_range().unwrap();
        assert_eq!(start, cell(0, 0));
        assert_eq!(end, cell(3, 10));
    }

    #[test]
    fn reverse_drag_normalizes_range() {
        let mut s = SelectionState::default();
        s.begin(cell(5, 20), Instant::now());
        s.extend(cell(2, 5));
        let (start, end) = s.char_range().unwrap();
        assert_eq!(start, cell(2, 5));
        assert_eq!(end, cell(5, 20));
    }

    #[test]
    fn commit_freezes_but_keeps_range() {
        let mut s = SelectionState::default();
        s.begin(cell(0, 0), Instant::now());
        s.extend(cell(0, 5));
        s.commit();
        assert!(s.is_frozen());
        assert!(s.is_active());
        assert_eq!(s.char_range().unwrap().1, cell(0, 5));
    }

    #[test]
    fn clear_wipes_selection() {
        let mut s = SelectionState::default();
        s.begin(cell(0, 0), Instant::now());
        s.extend(cell(0, 5));
        s.commit();
        s.clear();
        assert!(!s.is_active());
        assert!(s.char_range().is_none());
    }

    #[test]
    fn char_contains_single_row() {
        let mut s = SelectionState::default();
        s.begin(cell(3, 5), Instant::now());
        s.extend(cell(3, 10));
        assert!(s.contains(3, 5, 80));
        assert!(s.contains(3, 10, 80));
        assert!(!s.contains(3, 4, 80));
        assert!(!s.contains(3, 11, 80));
        assert!(!s.contains(4, 5, 80));
    }

    #[test]
    fn char_contains_multi_row_flows_first_and_last() {
        let mut s = SelectionState::default();
        s.begin(cell(2, 70), Instant::now());
        s.extend(cell(4, 3));
        // First row: 70..end.
        assert!(s.contains(2, 70, 80));
        assert!(s.contains(2, 79, 80));
        assert!(!s.contains(2, 69, 80));
        // Middle row: fully highlighted.
        assert!(s.contains(3, 0, 80));
        assert!(s.contains(3, 79, 80));
        // Last row: 0..=3.
        assert!(s.contains(4, 0, 80));
        assert!(s.contains(4, 3, 80));
        assert!(!s.contains(4, 4, 80));
    }

    #[test]
    fn line_mode_ignores_columns() {
        let mut s = SelectionState::default();
        let t0 = Instant::now();
        s.begin(cell(3, 5), t0);
        s.begin(cell(3, 5), t0 + Duration::from_millis(50));
        s.begin(cell(3, 5), t0 + Duration::from_millis(100));
        assert_eq!(s.granularity(), Granularity::Line);
        assert!(s.contains(3, 0, 80));
        assert!(s.contains(3, 79, 80));
        assert!(!s.contains(2, 5, 80));
        assert!(!s.contains(4, 5, 80));
    }

    #[test]
    fn shift_extend_promotes_to_char_if_no_anchor() {
        let mut s = SelectionState::default();
        s.shift_extend(cell(0, 5));
        assert!(s.is_active());
        assert_eq!(s.granularity(), Granularity::Char);
        assert_eq!(s.char_range().unwrap(), (cell(0, 5), cell(0, 5)));
    }

    #[test]
    fn is_word_char_recognises_punctuation_break() {
        assert!(is_word_char('a'));
        assert!(is_word_char('9'));
        assert!(is_word_char('/'));
        assert!(is_word_char('.'));
        assert!(is_word_char('-'));
        assert!(is_word_char('_'));
        assert!(!is_word_char(' '));
        assert!(!is_word_char('\t'));
        assert!(!is_word_char('('));
        assert!(!is_word_char(','));
    }
}
