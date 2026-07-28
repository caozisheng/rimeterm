//! Native Git pane.
//!
//! Read-only workspace Git panel backed by [`GitWorker`]. Renders a status
//! header, changes list, commit history, and (optionally) a diff pane.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Widget},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use crate::diff_highlight::DiffHighlighter;
use crate::git_model::{
    ChangeKind, ChangeSide, DiffLineOrigin, DiffSnapshot, GitChange, GitRequest, GitResponse,
    GitSnapshot,
};
use crate::git_worker::GitWorker;

/// Which sub-list currently has focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Changes,
    Commits,
    Diff,
}

/// Native GitPane provider.
pub struct GitPane {
    id: PaneId,
    title: String,
    worker: GitWorker,
    workspace_root: PathBuf,
    current_root: Option<PathBuf>,
    requested_generation: u64,
    applied_generation: u64,
    snapshot: GitSnapshot,
    diff: Option<DiffSnapshot>,
    focus: Focus,
    changes_cursor: usize,
    commits_cursor: usize,
    highlighter: DiffHighlighter,
}

impl GitPane {
    pub fn new(workspace_root: PathBuf) -> Self {
        let worker = GitWorker::spawn();
        let mut pane = Self {
            id: PaneId::next(),
            title: "Git".to_owned(),
            worker,
            workspace_root: workspace_root.clone(),
            current_root: None,
            requested_generation: 0,
            applied_generation: 0,
            snapshot: GitSnapshot::empty(0),
            diff: None,
            focus: Focus::Changes,
            changes_cursor: 0,
            commits_cursor: 0,
            highlighter: DiffHighlighter::new(),
        };
        pane.request_refresh_at(&workspace_root);
        pane
    }

    /// Ask the worker to snapshot a fresh directory (usually files-cwd changed).
    pub fn refresh_for(&mut self, cwd: &Path) {
        self.request_refresh_at(cwd);
    }

    /// Force a refresh of whatever directory we currently track.
    pub fn refresh(&mut self) {
        let target = self
            .current_root
            .clone()
            .unwrap_or_else(|| self.workspace_root.clone());
        self.request_refresh_at(&target);
    }

    fn request_refresh_at(&mut self, cwd: &Path) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(GitRequest::Snapshot {
            generation: self.requested_generation,
            cwd: cwd.to_path_buf(),
        });
    }

    fn selected_change(&self) -> Option<&GitChange> {
        self.snapshot.changes.get(self.changes_cursor)
    }

    fn request_diff(&mut self) {
        let Some(root) = self.current_root.clone() else {
            return;
        };
        let Some(change) = self.selected_change().cloned() else {
            return;
        };
        if matches!(change.side, ChangeSide::Staged) {
            return; // staged diffs deferred to a later patch
        }
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(GitRequest::WorktreeDiff {
            generation: self.requested_generation,
            repo_root: root,
            change,
        });
    }
}

impl PaneProvider for GitPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let border_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let title = git_title(&self.snapshot);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.height == 0 || inner.width == 0 {
            return RenderOutcome::default();
        }

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(inner);
        render_changes(
            frame,
            layout[0],
            &self.snapshot.changes,
            self.changes_cursor,
            self.focus,
        );
        render_commits(
            frame,
            layout[1],
            &self.snapshot.commits,
            self.commits_cursor,
            self.focus,
        );

        if let Some(diff) = &self.diff {
            render_diff_overlay(frame, inner, diff, &mut self.highlighter);
        }

        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Changes => Focus::Commits,
                    Focus::Commits => Focus::Diff,
                    Focus::Diff => Focus::Changes,
                };
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                match self.focus {
                    Focus::Changes => {
                        if !self.snapshot.changes.is_empty() {
                            self.changes_cursor =
                                (self.changes_cursor + 1).min(self.snapshot.changes.len() - 1);
                        }
                    }
                    Focus::Commits => {
                        if !self.snapshot.commits.is_empty() {
                            self.commits_cursor =
                                (self.commits_cursor + 1).min(self.snapshot.commits.len() - 1);
                        }
                    }
                    Focus::Diff => {}
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    Focus::Changes => self.changes_cursor = self.changes_cursor.saturating_sub(1),
                    Focus::Commits => self.commits_cursor = self.commits_cursor.saturating_sub(1),
                    Focus::Diff => {}
                }
                true
            }
            KeyCode::Enter => {
                if matches!(self.focus, Focus::Changes) {
                    self.request_diff();
                }
                true
            }
            KeyCode::Esc => {
                if self.diff.is_some() {
                    self.diff = None;
                    self.focus = Focus::Changes;
                }
                true
            }
            _ => false,
        }
    }

    fn reload(&mut self) {
        self.refresh();
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        for response in self.worker.drain() {
            match response {
                GitResponse::Snapshot(snapshot) => {
                    if snapshot.generation < self.applied_generation {
                        continue;
                    }
                    self.applied_generation = snapshot.generation;
                    self.current_root = snapshot.repo_root.clone();
                    if self.changes_cursor >= snapshot.changes.len() {
                        self.changes_cursor = snapshot.changes.len().saturating_sub(1);
                    }
                    if self.commits_cursor >= snapshot.commits.len() {
                        self.commits_cursor = snapshot.commits.len().saturating_sub(1);
                    }
                    self.snapshot = snapshot;
                    changed = true;
                }
                GitResponse::Diff(diff) => {
                    if diff.generation >= self.applied_generation {
                        self.applied_generation = diff.generation;
                        self.diff = Some(diff);
                        self.focus = Focus::Diff;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn git_title(snapshot: &GitSnapshot) -> String {
    match (&snapshot.head, &snapshot.upstream) {
        (Some(head), Some(upstream)) => format!(
            " 🌿 Git · {}  ↑{} ↓{} ",
            head.label, upstream.ahead, upstream.behind
        ),
        (Some(head), None) => format!(" 🌿 Git · {} ", head.label),
        (None, _) => " 🌿 Git · (not a repository) ".to_owned(),
    }
}

fn render_changes(
    frame: &mut Frame<'_>,
    area: Rect,
    changes: &[GitChange],
    cursor: usize,
    focus: Focus,
) {
    let items: Vec<ListItem<'_>> = changes
        .iter()
        .enumerate()
        .map(|(idx, change)| {
            let marker = if idx == cursor && matches!(focus, Focus::Changes) {
                "▶"
            } else {
                " "
            };
            let side = match change.side {
                ChangeSide::Staged => "S",
                ChangeSide::Worktree => "W",
            };
            let color = match change.kind {
                ChangeKind::Added | ChangeKind::Untracked => Color::Green,
                ChangeKind::Deleted => Color::Red,
                ChangeKind::Modified | ChangeKind::TypeChange => Color::Yellow,
                ChangeKind::Renamed => Color::Cyan,
                ChangeKind::Conflict => Color::Magenta,
            };
            let line = Line::from(vec![
                Span::raw(marker),
                Span::raw(" "),
                Span::styled(format!("[{side}]"), Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(change.kind.short().to_string(), Style::default().fg(color)),
                Span::raw(" "),
                Span::raw(change.path.display().to_string()),
            ]);
            ListItem::new(line)
        })
        .collect();
    let title = format!(" Changes ({}) ", changes.len());
    let border = if matches!(focus, Focus::Changes) {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    List::new(items)
        .block(block)
        .render(area, frame.buffer_mut());
}

fn render_commits(
    frame: &mut Frame<'_>,
    area: Rect,
    commits: &[crate::git_model::CommitSummary],
    cursor: usize,
    focus: Focus,
) {
    let items: Vec<ListItem<'_>> = commits
        .iter()
        .enumerate()
        .map(|(idx, commit)| {
            let marker = if idx == cursor && matches!(focus, Focus::Commits) {
                "▶"
            } else {
                " "
            };
            let line = Line::from(vec![
                Span::raw(marker),
                Span::raw(" "),
                Span::styled(&commit.short_id, Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::raw(commit.summary.clone()),
                Span::raw("  "),
                Span::styled(
                    format!("({})", commit.author),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();
    let title = format!(" Commits ({}) ", commits.len());
    let border = if matches!(focus, Focus::Commits) {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    List::new(items)
        .block(block)
        .render(area, frame.buffer_mut());
}

fn render_diff_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    diff: &DiffSnapshot,
    highlighter: &mut DiffHighlighter,
) {
    let block = Block::default()
        .title(" Diff (Esc to close) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    ratatui::widgets::Clear.render(area, frame.buffer_mut());
    block.render(area, frame.buffer_mut());
    let file = diff.files.first();
    let Some(file) = file else {
        return;
    };
    let ext = file
        .new_path
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let mut lines: Vec<Line<'_>> = Vec::new();
    for hunk in &file.hunks {
        lines.push(Line::styled(
            format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            ),
            Style::default().fg(Color::Blue),
        ));
        for line in &hunk.lines {
            let (marker, style) = match line.origin {
                DiffLineOrigin::Addition => ("+", Style::default().fg(Color::Green)),
                DiffLineOrigin::Removal => ("-", Style::default().fg(Color::Red)),
                DiffLineOrigin::Context => (" ", Style::default()),
            };
            let spans =
                highlight_line(ext, line.content.trim_end_matches('\n'), highlighter, style);
            let mut ordered = vec![Span::styled(marker, style)];
            ordered.extend(spans);
            lines.push(Line::from(ordered));
        }
    }
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

fn highlight_line<'a>(
    ext: &str,
    text: &'a str,
    highlighter: &mut DiffHighlighter,
    base: Style,
) -> Vec<Span<'a>> {
    if text.is_empty() {
        return vec![Span::raw("")];
    }
    let spans = highlighter.highlight(ext, text);
    spans
        .into_iter()
        .map(|span| {
            let slice = text.get(span.start..span.end).unwrap_or("");
            let style = span
                .label
                .map(|label| base.fg(color_for(label)))
                .unwrap_or(base);
            Span::styled(slice.to_owned(), style)
        })
        .collect()
}

fn color_for(label: &str) -> Color {
    match label {
        "keyword" => Color::Magenta,
        "type" => Color::Cyan,
        "string" => Color::Green,
        "number" => Color::LightBlue,
        "function" => Color::Yellow,
        "comment" => Color::DarkGray,
        "attribute" | "property" => Color::LightCyan,
        _ => Color::White,
    }
}
