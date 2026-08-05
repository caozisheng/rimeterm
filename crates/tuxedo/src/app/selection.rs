use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    selected: HashSet<usize>,
    editing: Option<usize>,
}

impl Selection {
    pub fn is_selected(&self, abs: usize) -> bool {
        self.selected.contains(&abs)
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected.iter().copied()
    }

    pub fn toggle(&mut self, abs: usize) {
        if !self.selected.insert(abs) {
            self.selected.remove(&abs);
        }
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    pub fn editing(&self) -> Option<usize> {
        self.editing
    }

    /// Enter edit mode on `abs`. Drops the multi-select set — editing one task
    /// while it's also flagged for bulk operations is structurally incoherent
    /// (`complete_selected` would double-handle the editing index).
    pub fn enter_edit(&mut self, abs: usize) {
        self.editing = Some(abs);
        self.selected.clear();
    }

    pub fn exit_edit(&mut self) {
        self.editing = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPoint {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    anchor: TextPoint,
    active: TextPoint,
    dragging: bool,
}

impl TextSelection {
    pub fn begin(point: TextPoint) -> Self {
        Self {
            anchor: point,
            active: point,
            dragging: true,
        }
    }

    pub fn update(&mut self, point: TextPoint) {
        self.active = point;
    }

    pub fn finish(&mut self) {
        self.dragging = false;
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn endpoints(self) -> (TextPoint, TextPoint) {
        if (self.anchor.line, self.anchor.column) <= (self.active.line, self.active.column) {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.active
    }

    pub fn contains(self, point: TextPoint) -> bool {
        let (start, end) = self.endpoints();
        (start.line, start.column) <= (point.line, point.column)
            && (point.line, point.column) < (end.line, end.column)
    }

    pub fn extract(self, lines: &[String]) -> String {
        let (start, end) = self.endpoints();
        if start == end {
            return String::new();
        }
        let mut output = String::new();
        for line_idx in start.line..=end.line {
            let Some(line) = lines.get(line_idx) else {
                break;
            };
            let from = if line_idx == start.line {
                start.column
            } else {
                0
            };
            let to = if line_idx == end.line {
                end.column
            } else {
                display_width(line)
            };
            output.push_str(&slice_display_columns(line, from, to));
            if line_idx != end.line {
                output.push('\n');
            }
        }
        output
    }
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn slice_display_columns(text: &str, from: usize, to: usize) -> String {
    let mut output = String::new();
    let mut column = 0;
    for ch in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        let next = column + width;
        let selected = if width == 0 {
            column >= from && column <= to
        } else {
            column >= from && next <= to
        };
        if selected {
            output.push(ch);
        }
        column = next;
    }
    output
}

#[cfg(test)]
mod text_tests {
    use super::{TextPoint, TextSelection};

    #[test]
    fn selection_normalizes_reverse_drag() {
        let mut selection = TextSelection::begin(TextPoint { line: 3, column: 4 });
        selection.update(TextPoint { line: 1, column: 2 });
        assert_eq!(
            selection.endpoints(),
            (
                TextPoint { line: 1, column: 2 },
                TextPoint { line: 3, column: 4 }
            )
        );
    }
    #[test]
    fn selection_extracts_utf8_by_display_column() {
        let mut selection = TextSelection::begin(TextPoint { line: 0, column: 1 });
        selection.update(TextPoint { line: 0, column: 3 });
        assert_eq!(selection.extract(&["a界b".into()]), "界");
    }
    #[test]
    fn selection_keeps_combining_marks() {
        let mut selection = TextSelection::begin(TextPoint { line: 0, column: 0 });
        selection.update(TextPoint { line: 0, column: 4 });
        assert_eq!(selection.extract(&["cafe\u{301}".into()]), "cafe\u{301}");
    }
}
