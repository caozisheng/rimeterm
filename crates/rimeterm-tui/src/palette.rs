//! Command palette overlay.
//!
//! §4.3 — a NativePane that pops on `Ctrl+Shift+P`, fuzzy-filters over
//! registered commands, and fires the selected one on `Enter`. Kept small and
//! dependency-free: fuzzy match is a simple subsequence with case-fold; if we
//! ever need speed we can slot in `fuzzy-matcher::SkimMatcherV2` later.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use rimeterm_core::command::{Command, CommandId};

#[derive(Debug, Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub cursor: usize,
}

impl PaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.cursor = 0;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.cursor = 0;
    }
}

#[derive(Debug, PartialEq)]
pub enum PaletteOutcome {
    Consumed,
    Closed,
    Run(CommandId),
    Passthrough,
}

/// Snapshot of a matched command — kept small so the caller can render / run.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub id: CommandId,
    pub title: &'static str,
    pub description: Option<&'static str>,
}

impl CommandEntry {
    pub fn from_command(c: &Command) -> Self {
        Self {
            id: c.id,
            title: c.title,
            description: c.description,
        }
    }
}

pub fn handle_key(
    state: &mut PaletteState,
    entries: &[CommandEntry],
    key: KeyEvent,
) -> PaletteOutcome {
    if !state.open {
        return PaletteOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
            state.close();
            PaletteOutcome::Closed
        }
        KeyCode::Up => {
            state.cursor = state.cursor.saturating_sub(1);
            PaletteOutcome::Consumed
        }
        KeyCode::Down => {
            let filtered = filter(entries, &state.query);
            if !filtered.is_empty() && state.cursor + 1 < filtered.len() {
                state.cursor += 1;
            }
            PaletteOutcome::Consumed
        }
        KeyCode::Enter => {
            let filtered = filter(entries, &state.query);
            if let Some(entry) = filtered.get(state.cursor) {
                let id = entry.id;
                state.close();
                PaletteOutcome::Run(id)
            } else {
                PaletteOutcome::Consumed
            }
        }
        KeyCode::Backspace => {
            state.query.pop();
            state.cursor = 0;
            PaletteOutcome::Consumed
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.push(c);
            state.cursor = 0;
            PaletteOutcome::Consumed
        }
        _ => PaletteOutcome::Consumed,
    }
}

/// Fuzzy filter that treats the query as a case-fold subsequence match over
/// `title`. Returns matches ordered by title length ascending (tie-break by id).
pub fn filter<'a>(entries: &'a [CommandEntry], query: &str) -> Vec<&'a CommandEntry> {
    if query.is_empty() {
        let mut out: Vec<&CommandEntry> = entries.iter().collect();
        out.sort_by(|a, b| a.title.cmp(b.title));
        return out;
    }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut hits: Vec<&CommandEntry> = entries
        .iter()
        .filter(|e| subsequence_match(e.title, &q))
        .collect();
    hits.sort_by(|a, b| {
        a.title
            .len()
            .cmp(&b.title.len())
            .then_with(|| a.id.cmp(b.id))
    });
    hits
}

fn subsequence_match(haystack: &str, needle: &[char]) -> bool {
    let mut idx = 0;
    for c in haystack.chars().flat_map(|c| c.to_lowercase()) {
        if idx < needle.len() && c == needle[idx] {
            idx += 1;
        }
    }
    idx == needle.len()
}

pub fn popup_rect(area: Rect) -> Rect {
    // Anchored to horizontal center, roughly a third of screen wide.
    let width = area.width.min(64).max(32);
    let height = 18.min(area.height.saturating_sub(2)).max(6);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = 2.min(area.height.saturating_sub(height));
    Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

pub fn render(area: Rect, buf: &mut Buffer, state: &PaletteState, entries: &[CommandEntry]) {
    if !state.open {
        return;
    }
    Clear.render(area, buf);
    let block = Block::default().title(" ⌘ palette ").borders(Borders::ALL);
    let inner = block.inner(area);
    block.render(area, buf);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    // Query line.
    let query_line = Line::from(vec![
        Span::styled("> ", Style::default().add_modifier(Modifier::DIM)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().add_modifier(Modifier::DIM)),
    ]);
    Paragraph::new(query_line).render(rows[0], buf);

    // Divider.
    Paragraph::new(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().add_modifier(Modifier::DIM),
    )))
    .render(rows[1], buf);

    // Results.
    let matches = filter(entries, &state.query);
    let list_area = rows[2];
    let max_rows = list_area.height as usize;
    let start = state.cursor.saturating_sub(max_rows.saturating_sub(1));
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(matches.len().min(max_rows));
    for (i, entry) in matches.iter().enumerate().skip(start).take(max_rows) {
        let selected = i == state.cursor;
        let style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        let mut spans = Vec::with_capacity(3);
        spans.push(Span::styled(format!(" {} ", entry.title), style));
        if let Some(desc) = entry.description {
            spans.push(Span::styled(
                format!(" — {}", desc),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        lines.push(Line::from(spans));
    }
    Paragraph::new(lines).render(list_area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &'static str, title: &'static str) -> CommandEntry {
        CommandEntry {
            id,
            title,
            description: None,
        }
    }

    #[test]
    fn empty_query_returns_sorted_by_title() {
        let entries = vec![entry("z", "zzz"), entry("a", "aaa")];
        let hits = filter(&entries, "");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "aaa");
    }

    #[test]
    fn subsequence_matches_regardless_of_case() {
        let entries = vec![
            entry("a", "Open Settings"),
            entry("b", "Show Acknowledgement"),
        ];
        let hits = filter(&entries, "OPS");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn nonmatching_returns_empty() {
        let entries = vec![entry("a", "Open Settings")];
        assert!(filter(&entries, "xyz").is_empty());
    }
}
