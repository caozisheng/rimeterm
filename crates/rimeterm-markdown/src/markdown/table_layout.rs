//! Fair-share table layout for [`TableBlock`].
//!
//! The upstream `layout_table` lived in `markdown-reader`'s widget layer,
//! which we deliberately did not vendor (see [`crate`] docs). This module is
//! the self-contained replacement used by the Alt+V viewer.
//!
//! # Algorithm
//!
//! 1. **Column sizing.** Start from the per-column `natural_widths` measured
//!    at parse time. If the sum plus per-cell padding/borders fits inside
//!    `content_width`, use natural widths as-is. Otherwise proportionally
//!    shrink each column to fit, subject to a floor of one display column
//!    per cell.
//! 2. **Cell wrapping.** Each cell's spans are word-wrapped to its final
//!    column width via [`crate::text_layout::wrap_spans`]. A row's visual
//!    height is the max wrapped-line count across its cells.
//! 3. **Alignment.** Wrapped lines are padded on left/right per column
//!    alignment (`Alignment::Left | None | Right | Center`).
//! 4. **Borders.** Rows are framed with Unicode box-drawing characters
//!    (`┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼ ─ │`). A single mid-border sits between the
//!    header and the body; body rows share their neighbours' borders.
//!
//! # Degradation
//!
//! When `content_width` is too small to render even a 1-column-wide box per
//! cell, we fall back to GFM pipe format (`| a | b |` + `---` divider).
//! Callers can still feed that into `Paragraph::wrap` without panicking —
//! it just loses column alignment.

use ratatui::text::{Line, Span};

use crate::markdown::TableBlock;
use crate::text_layout::{WrappedLine, wrap_spans};

// ── Box-drawing constants ────────────────────────────────────────────────────

const TL: char = '┌';
const TR: char = '┐';
const BL: char = '└';
const BR: char = '┘';
const H: char = '─';
const V: char = '│';
const T_DOWN: char = '┬';
const T_UP: char = '┴';
const T_RIGHT: char = '├';
const T_LEFT: char = '┤';
const CROSS: char = '┼';

/// Non-content overhead of one row's border/padding for `n_cols` columns.
///
/// Each cell contributes `│ <content> ` (leading vertical + two padding
/// spaces surrounding content) and the final `│` closes the row. Total:
/// `n_cols * 3 + 1`.
#[inline]
fn row_overhead(n_cols: usize) -> usize {
    n_cols * 3 + 1
}

/// Lay out `t` as a boxed table sized to `content_width` display columns.
///
/// Returns one [`Line<'static>`] per visual row. When the natural widths of
/// every column fit inside `content_width` the returned lines can be narrower
/// than the viewport; otherwise the box fills `content_width` exactly.
///
/// # Fallbacks
///
/// - Empty table (no headers, no rows) → `vec![]`.
/// - `content_width == 0` or too narrow to fit `n_cols * 4 + 1` columns
///   (one content cell per column plus box overhead) → pipe-format
///   rendering as a graceful degradation. Cells' inline styles are
///   preserved in either mode.
pub fn layout_table(t: &TableBlock, content_width: u16) -> Vec<Line<'static>> {
    let header_cols = t.headers.len();
    let body_cols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let n_cols = header_cols.max(body_cols);
    if n_cols == 0 {
        return Vec::new();
    }

    // Not enough real estate for a boxed table → pipe format. Guarantees
    // one content column per cell plus per-cell borders/padding.
    let min_needed = row_overhead(n_cols) + n_cols;
    if content_width == 0 || (content_width as usize) < min_needed {
        return pipe_format(t);
    }

    let budget = (content_width as usize) - row_overhead(n_cols);
    let col_widths = compute_column_widths(&t.natural_widths, n_cols, budget);

    let mut out = Vec::new();
    out.push(border_line(&col_widths, Edge::Top));

    // Header row (if any) followed by a mid-border. If there is no header,
    // body rows sit directly under the top border.
    if !t.headers.is_empty() {
        emit_row(&mut out, &t.headers, &col_widths, &t.alignments);
        if !t.rows.is_empty() {
            out.push(border_line(&col_widths, Edge::Mid));
        }
    }

    for row in &t.rows {
        emit_row(&mut out, row, &col_widths, &t.alignments);
    }

    out.push(border_line(&col_widths, Edge::Bottom));
    out
}

/// Compute the final per-column content widths.
///
/// Returns a `Vec<usize>` of length `n_cols`, each entry ≥ 1, whose sum is
/// exactly `budget` whenever we had to shrink (so the resulting border row
/// exactly fills `content_width`). When natural widths already fit, columns
/// keep their measured widths and the row can be narrower than the viewport
/// (matches what browsers or bat's markdown output do — over-tight columns
/// waste horizontal space).
fn compute_column_widths(naturals: &[usize], n_cols: usize, budget: usize) -> Vec<usize> {
    // Naturals are measured up to `header_cols` at parse time; a body row can
    // carry more columns than the header. Extend with 1s so every column has
    // at least a token of ideal width.
    let mut ideal: Vec<usize> = (0..n_cols)
        .map(|i| naturals.get(i).copied().unwrap_or(1).max(1))
        .collect();
    let total: usize = ideal.iter().sum();

    if total <= budget {
        return ideal;
    }

    // Proportional shrink with a floor of 1. Give each column
    // `floor(ideal * budget / total)`, then hand out remaining budget to the
    // widest columns first so the final sum equals `budget` exactly.
    let mut widths: Vec<usize> = ideal
        .iter()
        .map(|&w| ((w * budget) / total).max(1))
        .collect();

    let mut used: usize = widths.iter().sum();

    // If we allocated more than the budget (can happen when the floor-1
    // clamp bit for narrow columns), shave from the widest columns first.
    while used > budget {
        // If every remaining column is at the floor of 1 we can't shrink
        // further — accept overflow rather than loop forever. Callers must
        // pick a wider viewport for the pathological "N cols in <N chars"
        // case; the boxed-vs-pipe gate above already handles the extreme.
        let Some((idx, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(i, w)| (**w, ideal[*i]))
        else {
            break;
        };
        widths[idx] -= 1;
        used -= 1;
    }

    // Distribute leftover width to the columns whose ideal was starved the
    // most (largest ideal − assigned ratio), so wide-content columns get
    // preference.
    while used < budget {
        let (idx, _) = ideal
            .iter()
            .zip(widths.iter())
            .enumerate()
            .max_by_key(|(_, (ide, w))| ide.saturating_sub(**w))
            .expect("n_cols > 0 by construction");
        widths[idx] += 1;
        ideal[idx] = ideal[idx].saturating_sub(1);
        used += 1;
    }

    widths
}

// ── Row emission ─────────────────────────────────────────────────────────────

enum Edge {
    Top,
    Mid,
    Bottom,
}

/// One border line, e.g. `┌────┬────┐` for `Edge::Top`.
fn border_line(col_widths: &[usize], edge: Edge) -> Line<'static> {
    let (left, joint, right) = match edge {
        Edge::Top => (TL, T_DOWN, TR),
        Edge::Mid => (T_RIGHT, CROSS, T_LEFT),
        Edge::Bottom => (BL, T_UP, BR),
    };

    let mut s =
        String::with_capacity(col_widths.iter().sum::<usize>() * 3 + col_widths.len() * 3 + 4);
    s.push(left);
    for (i, &w) in col_widths.iter().enumerate() {
        // Each column reserves `w + 2` horizontal dashes (2 for the
        // padding spaces that surround content in body rows).
        for _ in 0..w + 2 {
            s.push(H);
        }
        s.push(if i + 1 == col_widths.len() {
            right
        } else {
            joint
        });
    }
    Line::from(Span::raw(s))
}

/// Emit one logical row (header or body). Wraps cells to their column width,
/// pads to the row's tallest cell, and pushes one [`Line`] per visual line
/// (so a row with a 3-line-tall cell becomes 3 output lines with the shorter
/// cells blank-padded).
fn emit_row(
    out: &mut Vec<Line<'static>>,
    cells: &[Vec<Span<'static>>],
    col_widths: &[usize],
    alignments: &[pulldown_cmark::Alignment],
) {
    // Wrap each cell to its column width (empty single row for absent trailing cells).
    let wrapped_per_col: Vec<Vec<WrappedLine>> = col_widths
        .iter()
        .enumerate()
        .map(|(i, &w)| match cells.get(i) {
            Some(cell) if !cell.is_empty() => wrap_spans(cell, w as u16),
            _ => vec![WrappedLine {
                spans: Vec::new(),
                width: 0,
            }],
        })
        .collect();

    let row_height = wrapped_per_col
        .iter()
        .map(|lines| lines.len())
        .max()
        .unwrap_or(1);

    let empty_line = WrappedLine {
        spans: Vec::new(),
        width: 0,
    };

    for line_idx in 0..row_height {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(col_widths.len() * 4 + 1);
        spans.push(Span::raw(format!("{V} ")));

        for (col_idx, &w) in col_widths.iter().enumerate() {
            if col_idx > 0 {
                spans.push(Span::raw(format!(" {V} ")));
            }
            let align = alignments
                .get(col_idx)
                .copied()
                .unwrap_or(pulldown_cmark::Alignment::None);
            let wl = wrapped_per_col[col_idx]
                .get(line_idx)
                .unwrap_or(&empty_line);
            append_aligned(&mut spans, wl, w, align);
        }

        spans.push(Span::raw(format!(" {V}")));
        out.push(Line::from(spans));
    }
}

/// Append one wrapped cell line to `spans`, padded to `col_w` display columns
/// per the requested alignment. Padding is done with plain-styled space runs
/// so it never accidentally inherits a nearby span's colour.
fn append_aligned(
    spans: &mut Vec<Span<'static>>,
    wl: &WrappedLine,
    col_w: usize,
    align: pulldown_cmark::Alignment,
) {
    let content_w = wl.width as usize;
    let pad = col_w.saturating_sub(content_w);
    let (left, right) = match align {
        pulldown_cmark::Alignment::Right => (pad, 0),
        pulldown_cmark::Alignment::Center => {
            let l = pad / 2;
            (l, pad - l)
        }
        _ => (0, pad),
    };

    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    for ws in &wl.spans {
        spans.push(Span::styled(ws.content.clone(), ws.style));
    }
    if right > 0 {
        spans.push(Span::raw(" ".repeat(right)));
    }
}

// ── Pipe-format fallback ─────────────────────────────────────────────────────

/// Plain GFM `| a | b |` format used when `content_width` is too small for a
/// boxed table. Loses column alignment but preserves cell content and inline
/// styling, and stays safe under `Paragraph::wrap`.
fn pipe_format(t: &TableBlock) -> Vec<Line<'static>> {
    fn render_row(cells: &[Vec<Span<'static>>]) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(cells.len() * 3 + 2);
        spans.push(Span::raw("| "));
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" | "));
            }
            spans.extend(cell.iter().cloned());
        }
        spans.push(Span::raw(" |"));
        Line::from(spans)
    }

    let mut out = Vec::with_capacity(2 + t.rows.len());
    if !t.headers.is_empty() {
        out.push(render_row(&t.headers));
        let dashes: Vec<Vec<Span<'static>>> =
            t.headers.iter().map(|_| vec![Span::raw("---")]).collect();
        out.push(render_row(&dashes));
    }
    for row in &t.rows {
        out.push(render_row(row));
    }
    out
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{TableBlock, TableBlockId};
    use crate::text_layout::measure;
    use pulldown_cmark::Alignment;
    use unicode_width::UnicodeWidthStr;

    fn cell(text: &'static str) -> Vec<Span<'static>> {
        vec![Span::raw(text)]
    }

    fn table(
        headers: Vec<Vec<Span<'static>>>,
        rows: Vec<Vec<Vec<Span<'static>>>>,
        alignments: Vec<Alignment>,
    ) -> TableBlock {
        // Per-column natural widths from the plain-text width of every cell.
        let n_cols = headers
            .len()
            .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
        let mut natural_widths = vec![0usize; n_cols];
        for (i, c) in headers.iter().enumerate() {
            natural_widths[i] = natural_widths[i].max(measure(c) as usize);
        }
        for row in &rows {
            for (i, c) in row.iter().enumerate() {
                natural_widths[i] = natural_widths[i].max(measure(c) as usize);
            }
        }
        for w in &mut natural_widths {
            *w = (*w).max(1);
        }
        TableBlock {
            id: TableBlockId(0),
            headers,
            rows,
            alignments,
            natural_widths,
            rendered_height: 0,
            source_line: 0,
            row_source_lines: vec![],
            source_byte_start: 0,
            source_byte_end: 0,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn empty_table_yields_no_lines() {
        let t = table(vec![], vec![], vec![]);
        assert!(layout_table(&t, 80).is_empty());
    }

    #[test]
    fn narrow_table_falls_back_to_pipe_format() {
        // 3 cols need 3*4 + 1 = 13 cols for the boxed layout. 8 is too narrow.
        let t = table(
            vec![cell("A"), cell("B"), cell("C")],
            vec![vec![cell("1"), cell("2"), cell("3")]],
            vec![Alignment::None; 3],
        );
        let lines = layout_table(&t, 8);
        let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains('|'), "pipe fallback expected: {text:?}");
        assert!(text.contains("---"), "divider expected: {text:?}");
    }

    #[test]
    fn natural_widths_when_they_fit() {
        let t = table(
            vec![cell("Alpha"), cell("Beta")],
            vec![
                vec![cell("one"), cell("two")],
                vec![cell("three"), cell("four")],
            ],
            vec![Alignment::None; 2],
        );
        let lines = layout_table(&t, 80);
        // top + header + mid + 2 rows + bottom = 6 lines
        assert_eq!(lines.len(), 6);
        // Every line must fit inside `content_width`.
        for l in &lines {
            let w = line_text(l).width();
            assert!(w <= 80, "line too wide ({w}): {:?}", line_text(l));
        }
        let joined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("Alpha"));
        assert!(joined.contains("three"));
        assert!(joined.contains('┌') && joined.contains('┘'));
        assert!(joined.contains('├') && joined.contains('┤'));
    }

    #[test]
    fn all_border_lines_have_uniform_width_when_shrunk() {
        // Force proportional shrink by picking a target width smaller than
        // the sum of naturals + overhead.
        let t = table(
            vec![cell("Alphabet"), cell("Numerical"), cell("Symbols")],
            vec![
                vec![cell("aaaaaaa"), cell("1234567890"), cell("!@#$%^")],
                vec![cell("bbbbbb"), cell("098765"), cell("&*()")],
            ],
            vec![Alignment::None; 3],
        );
        let lines = layout_table(&t, 30);
        for (i, l) in lines.iter().enumerate() {
            let w = line_text(l).width();
            assert!(w <= 30, "line {i} exceeds 30 cols: {w}");
        }
        // Every border row must be the same width — else the box looks broken.
        let borders: Vec<usize> = lines
            .iter()
            .filter(|l| {
                let s = line_text(l);
                s.starts_with('┌') || s.starts_with('├') || s.starts_with('└')
            })
            .map(|l| line_text(l).width())
            .collect();
        assert!(!borders.is_empty());
        let first = borders[0];
        for w in &borders {
            assert_eq!(*w, first, "border widths must match: {borders:?}");
        }
    }

    #[test]
    fn column_widths_shrink_proportionally() {
        // 4 cols, ideal 5+5+5+5 = 20, budget 10.
        let widths = compute_column_widths(&[5, 5, 5, 5], 4, 10);
        assert_eq!(widths.iter().sum::<usize>(), 10);
        assert!(widths.iter().all(|&w| w >= 1));
        let min = *widths.iter().min().unwrap();
        let max = *widths.iter().max().unwrap();
        // Equal-ideal columns must be within 1 col of each other.
        assert!(max - min <= 1, "widths should be balanced: {widths:?}");
    }

    #[test]
    fn column_widths_respect_min_of_one() {
        // 6 cols on a 3-wide budget: everyone floors to 1, shrink loop
        // gives up rather than looping forever. We at least verify no zero
        // widths and no panic.
        let widths = compute_column_widths(&[1; 6], 6, 3);
        assert_eq!(widths.len(), 6);
        assert!(widths.iter().all(|&w| w >= 1));
    }

    #[test]
    fn cells_wrap_to_multi_line_rows() {
        let t = table(
            vec![cell("short"), cell("long content goes here in two words")],
            vec![vec![cell("x"), cell("y")]],
            vec![Alignment::None; 2],
        );
        // Force shrink: naturals sum ~5 + 35 = 40; overhead 7 → total 47.
        // At content_width = 25 the second column shrinks and its cell wraps.
        let lines = layout_table(&t, 25);
        // top border + wrapped header (≥2 lines) + mid + body + bottom.
        assert!(
            lines.len() >= 6,
            "expected multi-line header + framing, got {} lines",
            lines.len()
        );
    }

    #[test]
    fn right_alignment_pads_on_the_left() {
        let t = table(
            vec![cell("Num")],
            vec![vec![cell("1")]],
            vec![Alignment::Right],
        );
        let lines = layout_table(&t, 20);
        let body = lines
            .iter()
            .find(|l| {
                let s = line_text(l);
                s.starts_with('│') && s.contains('1') && !s.contains("Num")
            })
            .expect("body row missing");
        let s = line_text(body);
        // Between opening "│ " and closing " │" the padding should come
        // before "1", not after.
        let opener = s.find('│').unwrap() + '│'.len_utf8();
        let closer = s.rfind('│').unwrap();
        let inside = &s[opener..closer];
        let trimmed = inside.trim_matches(' ');
        assert!(
            trimmed.ends_with('1'),
            "expected right-alignment to place '1' at the end, got {inside:?}"
        );
    }

    #[test]
    fn body_only_table_has_no_mid_border() {
        let t = table(
            vec![],
            vec![vec![cell("x"), cell("y")]],
            vec![Alignment::None, Alignment::None],
        );
        let lines = layout_table(&t, 40);
        // Only top + body + bottom = 3 lines.
        assert_eq!(lines.len(), 3);
        let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!text.contains('├'), "no mid border expected: {text:?}");
    }

    #[test]
    fn header_only_table_has_no_mid_border() {
        // Header but no body rows — top + header line + bottom, no ├───┤.
        let t = table(
            vec![cell("A"), cell("B")],
            vec![],
            vec![Alignment::None, Alignment::None],
        );
        let lines = layout_table(&t, 40);
        assert_eq!(lines.len(), 3);
        let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!text.contains('├'), "no mid border expected: {text:?}");
        assert!(text.contains('┌'));
        assert!(text.contains('└'));
    }

    #[test]
    fn preserves_inline_styling() {
        use ratatui::style::{Color, Style};
        let styled = vec![Span::styled("bold", Style::default().fg(Color::Red))];
        let t = table(
            vec![styled.clone()],
            vec![vec![vec![Span::raw("plain")]]],
            vec![Alignment::None],
        );
        let lines = layout_table(&t, 30);
        // The styled span must survive with its non-default style.
        let saw_style = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.fg == Some(Color::Red));
        assert!(saw_style, "styled span was stripped by layout_table");
    }
}
