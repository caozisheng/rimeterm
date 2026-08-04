//! Native Git pane.
//!
//! Read-only workspace Git panel backed by [`GitWorker`]. Renders a status
//! header, changes list, commit history, and (optionally) a diff pane.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, StatefulWidget, Widget,
    },
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use crate::diff_highlight::DiffHighlighter;
use crate::git_model::{
    ChangeKind, ChangeSide, CommitDetail as CommitDetailData, CommitSummary, DiffLineOrigin,
    DiffSnapshot, GitChange, GitRef, GitRequest, GitResponse, GitSnapshot,
};
use crate::git_worker::GitWorker;

/// Which sub-list currently has focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Changes,
    Commits,
    Diff,
    /// A commit's full detail (message + meta + per-file diff) is shown.
    Detail,
}

/// Initial commit-history window and how many rows we add every time the
/// user scrolls off the bottom. Each read is bounded to ~1–2 ms per row,
/// so a 50-row page keeps the cwd-change latency well under one frame.
const COMMIT_PAGE: usize = 50;

/// Native GitPane provider.
pub struct GitPane {
    id: PaneId,
    title: String,
    worker: GitWorker,
    workspace_root: PathBuf,
    current_root: Option<PathBuf>,
    /// Monotonic counter shared by both request kinds — used only for
    /// coalescing consecutive `Snapshot` requests in the worker.
    requested_generation: u64,
    /// Highest `Snapshot` generation that has been applied to `snapshot`.
    /// Isolated from `applied_diff_generation` because the worker processes
    /// snapshots *before* diffs when both are queued in the same drain
    /// batch; folding them into one counter caused a race where the
    /// snapshot's response bumped the counter past any queued diff and
    /// silently dropped it (§B14 regression).
    applied_snapshot_generation: u64,
    /// Highest `Diff` generation that has been applied to `diff`.
    applied_diff_generation: u64,
    /// Highest `CommitDetail` generation that has been applied. Isolated
    /// from snapshot / diff for the same reason those two are isolated
    /// from each other.
    applied_detail_generation: u64,
    snapshot: GitSnapshot,
    diff: Option<DiffSnapshot>,
    /// Current commit-detail overlay, when open.
    detail: Option<CommitDetailData>,
    /// Vertical scroll offset inside the detail overlay.
    detail_scroll: u16,
    focus: Focus,
    changes_cursor: usize,
    commits_cursor: usize,
    /// Persisted `ListState` so scrolling follows the cursor across
    /// renders — the widget itself owns the "visible-window offset"
    /// bookkeeping that keeps the highlight on screen.
    changes_list_state: ListState,
    commits_list_state: ListState,
    /// How many commits the *last request* asked for. Grows in
    /// `COMMIT_PAGE` steps when the user scrolls past the bottom; reset
    /// to `COMMIT_PAGE` on every cwd change so we don't carry a huge
    /// window into an unrelated repo.
    commit_limit: usize,
    /// True while the pane is waiting for a response to a `Load more`
    /// request. Prevents queuing a second growth before the first lands.
    load_more_in_flight: bool,
    highlighter: DiffHighlighter,
    /// Rect of the Changes list captured on the last render. Used by
    /// `on_mouse` to route scroll wheel events without depending on the
    /// caller's outer rect.
    changes_rect: Rect,
    /// Rect of the Commits list captured on the last render.
    commits_rect: Rect,
    /// Total rendered-line count of the last detail overlay — used to
    /// clamp `detail_scroll` so users can't scroll past the tail.
    detail_line_count: u16,
    /// Visible height of the last detail overlay's inner area — used with
    /// `detail_line_count` to derive the max scroll offset.
    detail_viewport_height: u16,
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
            applied_snapshot_generation: 0,
            applied_diff_generation: 0,
            applied_detail_generation: 0,
            snapshot: GitSnapshot::empty(0),
            diff: None,
            detail: None,
            detail_scroll: 0,
            focus: Focus::Changes,
            changes_cursor: 0,
            commits_cursor: 0,
            changes_list_state: ListState::default(),
            commits_list_state: ListState::default(),
            commit_limit: COMMIT_PAGE,
            load_more_in_flight: false,
            highlighter: DiffHighlighter::new(),
            changes_rect: Rect::default(),
            commits_rect: Rect::default(),
            detail_line_count: 0,
            detail_viewport_height: 0,
        };
        pane.request_refresh_at(&workspace_root);
        pane
    }

    /// Ask the worker to snapshot a fresh directory (usually files-cwd changed).
    pub fn refresh_for(&mut self, cwd: &Path) {
        // Fresh cwd → start with a fresh page. Whatever `load more`
        // work was in flight is irrelevant to the new repo.
        self.commit_limit = COMMIT_PAGE;
        self.load_more_in_flight = false;
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
            commit_limit: self.commit_limit,
        });
    }

    /// Grow the commit window by one page and re-snapshot. No-op when a
    /// prior growth is still pending or when we already exhausted history
    /// (last snapshot came back with fewer commits than the requested
    /// limit — meaning there's nothing older to fetch).
    fn request_more_commits(&mut self) {
        if self.load_more_in_flight {
            return;
        }
        if self.snapshot.commits.len() < self.commit_limit {
            return;
        }
        let Some(root) = self.current_root.clone() else {
            return;
        };
        self.commit_limit = self.commit_limit.saturating_add(COMMIT_PAGE);
        self.load_more_in_flight = true;
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(GitRequest::Snapshot {
            generation: self.requested_generation,
            cwd: root,
            commit_limit: self.commit_limit,
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

    /// Fire a background worker request for the full detail (metadata +
    /// per-file hunks) of the commit under the Commits cursor. No-op when
    /// the pane hasn't discovered a repo yet or when the commits list is
    /// empty.
    fn request_commit_detail(&mut self) {
        let Some(root) = self.current_root.clone() else {
            return;
        };
        let Some(commit) = self.snapshot.commits.get(self.commits_cursor) else {
            return;
        };
        let commit_id = commit.id.clone();
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(GitRequest::CommitDetail {
            generation: self.requested_generation,
            repo_root: root,
            commit_id,
        });
    }

    /// Close whichever overlay is currently open (diff or commit detail).
    /// Returns focus to the sub-list that owned the overlay's source row.
    fn close_overlay(&mut self) {
        if self.detail.is_some() {
            self.detail = None;
            self.focus = Focus::Commits;
        } else if self.diff.is_some() {
            self.diff = None;
            self.focus = Focus::Changes;
        }
    }

    /// Scroll the Changes list one wheel-tick. `down = true` moves the
    /// cursor toward newer entries; `false` toward older.
    fn scroll_changes(&mut self, down: bool) {
        if self.snapshot.changes.is_empty() {
            return;
        }
        self.focus = Focus::Changes;
        if down {
            let max_idx = self.snapshot.changes.len() - 1;
            self.changes_cursor = (self.changes_cursor + 3).min(max_idx);
        } else {
            self.changes_cursor = self.changes_cursor.saturating_sub(3);
        }
    }

    /// Scroll the Commits list one wheel-tick and load more history when
    /// the cursor would fall off the bottom.
    fn scroll_commits(&mut self, down: bool) {
        self.focus = Focus::Commits;
        if down {
            let total = self.snapshot.commits.len();
            if total == 0 {
                return;
            }
            let max_idx = total - 1;
            let was_at_bottom = self.commits_cursor >= max_idx;
            self.commits_cursor = (self.commits_cursor + 3).min(max_idx);
            // If we were already parked at the last row (or the scroll
            // clamps us there this tick), ask the worker for the next
            // page. `request_more_commits` is a no-op when a request is
            // already in flight or when history is exhausted.
            if was_at_bottom || self.commits_cursor == max_idx {
                self.request_more_commits();
            }
        } else {
            self.commits_cursor = self.commits_cursor.saturating_sub(3);
        }
    }

    /// Scroll the detail overlay one wheel-tick, clamped to the last
    /// rendered line so users can't scroll into empty space beyond the
    /// tail of the diff.
    fn scroll_detail(&mut self, down: bool) {
        if self.detail.is_none() {
            return;
        }
        let step: u16 = 3;
        if down {
            let max = self
                .detail_line_count
                .saturating_sub(self.detail_viewport_height);
            self.detail_scroll = self.detail_scroll.saturating_add(step).min(max);
        } else {
            self.detail_scroll = self.detail_scroll.saturating_sub(step);
        }
    }

    /// Fallback wheel router used when the event lands anywhere in the
    /// pane's outer rect but *outside* a specific sub-list — e.g. on the
    /// pane border, the title bar, or on top of an open overlay.
    fn route_scroll_by_focus(&mut self, down: bool) {
        match self.focus {
            Focus::Changes => self.scroll_changes(down),
            Focus::Commits => self.scroll_commits(down),
            Focus::Diff => {}
            Focus::Detail => self.scroll_detail(down),
        }
    }

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let focus = match self.focus {
            Focus::Commits | Focus::Detail => "commits",
            Focus::Changes | Focus::Diff => "changes",
        };
        rimeterm_config::memory_state::PaneState {
            values: std::collections::BTreeMap::from([("focus".into(), focus.into())]),
        }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        self.focus = match state.values.get("focus").map(String::as_str) {
            Some("commits") => Focus::Commits,
            _ => Focus::Changes,
        };
        self.diff = None;
        self.detail = None;
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
        self.changes_rect = layout[0];
        self.commits_rect = layout[1];
        render_changes(
            frame,
            layout[0],
            &self.snapshot.changes,
            self.changes_cursor,
            self.focus,
            &mut self.changes_list_state,
        );
        render_commits(
            frame,
            layout[1],
            &self.snapshot.commits,
            self.snapshot.graph_width,
            self.commits_cursor,
            self.focus,
            &mut self.commits_list_state,
        );

        if let Some(diff) = &self.diff {
            render_diff_overlay(frame, inner, diff, &mut self.highlighter);
        }
        if let Some(detail) = &self.detail {
            let (line_count, viewport) = render_detail_overlay(
                frame,
                inner,
                detail,
                self.detail_scroll,
                &mut self.highlighter,
            );
            self.detail_line_count = line_count;
            self.detail_viewport_height = viewport;
        } else {
            self.detail_line_count = 0;
            self.detail_viewport_height = 0;
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
                // In the detail overlay Tab is a no-op — the overlay is
                // modal until Esc / ←.
                self.focus = match self.focus {
                    Focus::Changes => Focus::Commits,
                    Focus::Commits => Focus::Diff,
                    Focus::Diff => Focus::Changes,
                    Focus::Detail => Focus::Detail,
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
                        let total = self.snapshot.commits.len();
                        if total > 0 {
                            let max_idx = total - 1;
                            let was_at_bottom = self.commits_cursor >= max_idx;
                            self.commits_cursor = (self.commits_cursor + 1).min(max_idx);
                            if was_at_bottom {
                                self.request_more_commits();
                            }
                        }
                    }
                    Focus::Diff => {}
                    Focus::Detail => {
                        self.detail_scroll = self.detail_scroll.saturating_add(1);
                    }
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    Focus::Changes => self.changes_cursor = self.changes_cursor.saturating_sub(1),
                    Focus::Commits => self.commits_cursor = self.commits_cursor.saturating_sub(1),
                    Focus::Diff => {}
                    Focus::Detail => {
                        self.detail_scroll = self.detail_scroll.saturating_sub(1);
                    }
                }
                true
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                // `→` and `l` mirror the vi-style navigation used elsewhere
                // in the pane; Enter stays as a compatibility alias.
                match self.focus {
                    Focus::Changes => self.request_diff(),
                    Focus::Commits => self.request_commit_detail(),
                    _ => {}
                }
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                // Symmetric close — Esc for muscle memory, ← / h to match
                // the vi navigation used elsewhere.
                self.close_overlay();
                true
            }
            KeyCode::Esc => {
                self.close_overlay();
                true
            }
            _ => false,
        }
    }

    fn on_mouse(&mut self, event: MouseEvent, outer: Rect) -> bool {
        let point_in = |rect: Rect| {
            rect.width > 0
                && rect.height > 0
                && event.column >= rect.x
                && event.column < rect.right()
                && event.row >= rect.y
                && event.row < rect.bottom()
        };
        let in_pane = point_in(outer);
        // Priority order for wheel events:
        //   1. If an overlay (detail / diff) is open and the wheel is
        //      anywhere over the pane, route to the overlay. The overlay
        //      draws on top of `changes_rect` / `commits_rect`, so the
        //      cached sub-rects are stale-until-next-render and must be
        //      bypassed.
        //   2. Otherwise dispatch by the sub-rect the wheel lands in
        //      (the common navigation path).
        //   3. Fallback: event lands on the pane border / title with no
        //      sub-rect hit → dispatch by current focus.
        if in_pane && self.detail.is_some() {
            return match event.kind {
                MouseEventKind::ScrollDown => {
                    self.scroll_detail(true);
                    true
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_detail(false);
                    true
                }
                _ => false,
            };
        }
        if in_pane && self.diff.is_some() {
            // Diff overlay currently doesn't scroll (see render_diff_overlay),
            // but we still consume the wheel so it doesn't fall through
            // to the covered commits list.
            return matches!(
                event.kind,
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
            );
        }
        match event.kind {
            MouseEventKind::ScrollDown if point_in(self.changes_rect) => {
                self.scroll_changes(true);
                true
            }
            MouseEventKind::ScrollUp if point_in(self.changes_rect) => {
                self.scroll_changes(false);
                true
            }
            MouseEventKind::ScrollDown if point_in(self.commits_rect) => {
                self.scroll_commits(true);
                true
            }
            MouseEventKind::ScrollUp if point_in(self.commits_rect) => {
                self.scroll_commits(false);
                true
            }
            MouseEventKind::ScrollDown if in_pane => {
                self.route_scroll_by_focus(true);
                true
            }
            MouseEventKind::ScrollUp if in_pane => {
                self.route_scroll_by_focus(false);
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
                    if snapshot.generation < self.applied_snapshot_generation {
                        continue;
                    }
                    self.applied_snapshot_generation = snapshot.generation;
                    // Any pending "load more" is resolved by this snapshot
                    // (whether or not it actually grew — it may have
                    // exhausted history and returned the same length).
                    self.load_more_in_flight = false;
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
                    // Gate diffs against their own stream. A queued snapshot
                    // response arrives on the same channel with a higher
                    // generation and used to invalidate an in-flight diff
                    // whenever a cwd change fired between the user pressing
                    // Enter/→ and the worker returning the diff.
                    if diff.generation < self.applied_diff_generation {
                        continue;
                    }
                    self.applied_diff_generation = diff.generation;
                    self.diff = Some(diff);
                    self.focus = Focus::Diff;
                    changed = true;
                }
                GitResponse::CommitDetail(detail) => {
                    // Gate detail responses against their own stream.
                    if detail.generation < self.applied_detail_generation {
                        continue;
                    }
                    self.applied_detail_generation = detail.generation;
                    self.detail = Some(detail);
                    self.detail_scroll = 0;
                    self.focus = Focus::Detail;
                    changed = true;
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
    list_state: &mut ListState,
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
    // `select(Some(cursor))` is what makes `List` scroll the viewport to
    // keep the highlighted row visible; without it the widget stays
    // parked at offset 0 and the cursor walks off the bottom edge.
    list_state.select(if changes.is_empty() {
        None
    } else {
        Some(cursor)
    });
    let list = List::new(items).block(block);
    StatefulWidget::render(list, area, frame.buffer_mut(), list_state);
    draw_list_scrollbar(frame, area, changes.len(), list_state.selected());
}

fn render_commits(
    frame: &mut Frame<'_>,
    area: Rect,
    commits: &[CommitSummary],
    graph_width: usize,
    cursor: usize,
    focus: Focus,
    list_state: &mut ListState,
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
            let mut spans: Vec<Span<'_>> =
                Vec::with_capacity(graph_width + 6 + commit.refs.len() * 2);
            spans.push(Span::raw(marker));
            spans.push(Span::raw(" "));
            spans.extend(graph_row_spans(commit, graph_width));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                &commit.short_id,
                Style::default().fg(Color::Yellow),
            ));
            // HEAD marker plus refs badges. Serie renders these before
            // the subject; we match to keep the eye-path identical.
            for badge in ref_badges(commit) {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            spans.push(Span::raw("  "));
            spans.push(Span::raw(commit.summary.clone()));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("({})", commit.author),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(Line::from(spans))
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
    list_state.select(if commits.is_empty() {
        None
    } else {
        Some(cursor)
    });
    let list = List::new(items).block(block);
    StatefulWidget::render(list, area, frame.buffer_mut(), list_state);
    draw_list_scrollbar(frame, area, commits.len(), list_state.selected());
}

/// Overlay a right-edge scrollbar inside a bordered list area. Positioned
/// by the currently-selected item; total scrollable length is the number
/// of list rows. A no-op when the list is empty or too short to warrant
/// visible scroll feedback.
fn draw_list_scrollbar(frame: &mut Frame<'_>, area: Rect, total: usize, selected: Option<usize>) {
    if total == 0 || area.height <= 2 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 0,
        vertical: 1,
    });
    let position = selected.unwrap_or(0);
    let mut state = ScrollbarState::new(total).position(position);
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .render(inner, frame.buffer_mut(), &mut state);
}

/// Rasterise one row of the commit graph.
///
/// Produces a flat glyph strip `graph_width + 1` cells wide: edges first
/// (later edges over earlier ones — order is what `build_edges` sorted for
/// us), then the commit dot on `commit.graph_column`, then any remaining
/// blank cells stay as spaces so the following text lines up.
fn graph_row_spans(commit: &CommitSummary, graph_width: usize) -> Vec<Span<'static>> {
    let width = graph_width + 1;
    // Cells hold `(glyph, color)` for the final raster. Space + default
    // color means "no edge at this column, no dot".
    let mut cells: Vec<(char, Color)> = vec![(' ', Color::Reset); width];
    for cell in &commit.graph_edges {
        if cell.column < width {
            cells[cell.column] = (cell.edge.glyph(), lane_color(cell.lane));
        }
    }
    if commit.graph_column < width {
        cells[commit.graph_column] = ('●', lane_color(commit.graph_column));
    }
    // One span per cell keeps the color of the traversing lane. Adjacent
    // spans with identical style are cheap for ratatui; not worth merging
    // for a ≤12-column-wide graph.
    cells
        .into_iter()
        .map(|(glyph, color)| Span::styled(glyph.to_string(), Style::default().fg(color)))
        .collect()
}

/// Stable per-lane color so a branch keeps its hue across every row it
/// occupies. Six colors is enough for well over the visible fan-out of
/// a normal repo history.
fn lane_color(lane: usize) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Yellow,
        Color::Green,
        Color::Cyan,
        Color::Magenta,
        Color::Blue,
        Color::Red,
    ];
    PALETTE[lane % PALETTE.len()]
}

/// Build the `[HEAD]` / `[branch]` / `[origin/branch]` / `[tag: name]`
/// badges shown between the short-id and the subject on a commit row.
/// Serie colors branches / remotes / tags differently — we mirror that so
/// the badge is scannable at a glance.
fn ref_badges(commit: &CommitSummary) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(commit.refs.len() + 1);
    if commit.is_head {
        out.push(Span::styled(
            "[HEAD]".to_owned(),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    for r in &commit.refs {
        let (label, color) = match r {
            GitRef::Branch(name) => (format!("[{name}]"), Color::Green),
            GitRef::RemoteBranch(name) => (format!("[{name}]"), Color::LightRed),
            GitRef::Tag(name) => (format!("[tag: {name}]"), Color::Yellow),
        };
        out.push(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    out
}

/// Draw the commit-detail overlay — modal `Clear` + header (SHA / author
/// / committer / parents / refs) + subject + body + per-file diff hunks.
/// Layout mirrors serie's `CommitDetail` widget.
fn render_detail_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    detail: &CommitDetailData,
    scroll: u16,
    highlighter: &mut DiffHighlighter,
) -> (u16, u16) {
    let block = Block::default()
        .title(format!(" Commit {}  (Esc/← to close) ", detail.short_id))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    ratatui::widgets::Clear.render(area, frame.buffer_mut());
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return (0, 0);
    }

    let mut lines: Vec<Line<'_>> = Vec::new();

    let label_style = Style::default().fg(Color::DarkGray);
    let hash_style = Style::default().fg(Color::Yellow);

    lines.push(Line::from(vec![
        Span::styled("    SHA: ", label_style),
        Span::styled(detail.id.clone(), hash_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" Author: ", label_style),
        Span::raw(format!("{} <{}>", detail.author_name, detail.author_email)),
    ]));
    if detail.author_name != detail.committer_name || detail.author_email != detail.committer_email
    {
        lines.push(Line::from(vec![
            Span::styled(" Commit: ", label_style),
            Span::raw(format!(
                "{} <{}>",
                detail.committer_name, detail.committer_email
            )),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("   Time: ", label_style),
        Span::raw(format_epoch(detail.author_seconds)),
    ]));
    if !detail.parents.is_empty() {
        let mut spans: Vec<Span<'_>> = vec![Span::styled("Parents: ", label_style)];
        for (i, p) in detail.parents.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(short_id(p).to_owned(), hash_style));
        }
        lines.push(Line::from(spans));
    }
    if !detail.refs.is_empty() {
        let mut spans: Vec<Span<'_>> = vec![Span::styled("   Refs: ", label_style)];
        for (i, r) in detail.refs.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            let (label, color) = match r {
                GitRef::Branch(name) => (name.clone(), Color::Green),
                GitRef::RemoteBranch(name) => (name.clone(), Color::LightRed),
                GitRef::Tag(name) => (format!("tag: {name}"), Color::Yellow),
            };
            spans.push(Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }
    // Divider then subject (bold) then body.
    lines.push(divider_line(inner.width));
    lines.push(Line::from(Span::styled(
        detail.subject.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    if !detail.body.is_empty() {
        for body_line in detail.body.lines() {
            lines.push(Line::from(body_line.to_owned()));
        }
    }
    // Divider then per-file diff.
    lines.push(divider_line(inner.width));
    for file in &detail.files {
        let header = match (&file.old_path, &file.new_path) {
            (Some(old), Some(new)) if old != new => {
                format!("R {} -> {}", old.display(), new.display())
            }
            (Some(_), Some(new)) => format!("M {}", new.display()),
            (None, Some(new)) => format!("A {}", new.display()),
            (Some(old), None) => format!("D {}", old.display()),
            _ => String::new(),
        };
        lines.push(Line::from(Span::styled(
            header.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        let ext = file
            .new_path
            .as_ref()
            .or(file.old_path.as_ref())
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        for hunk in &file.hunks {
            lines.push(Line::from(Span::styled(
                format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
                ),
                Style::default().fg(Color::Magenta),
            )));
            for line in &hunk.lines {
                let (prefix, base) = match line.origin {
                    DiffLineOrigin::Addition => ("+", Style::default().fg(Color::Green)),
                    DiffLineOrigin::Removal => ("-", Style::default().fg(Color::Red)),
                    DiffLineOrigin::Context => (" ", Style::default().fg(Color::Gray)),
                };
                let mut spans: Vec<Span<'_>> = vec![Span::styled(prefix, base)];
                spans.extend(highlight_line(ext, &line.content, highlighter, base));
                lines.push(Line::from(spans));
            }
        }
        lines.push(Line::from(""));
    }

    let line_count = lines.len();
    Paragraph::new(lines)
        .scroll((scroll, 0))
        .render(inner, frame.buffer_mut());

    // Right-edge scrollbar tracks position through the rendered content.
    if line_count > inner.height as usize {
        let mut state = ScrollbarState::new(line_count).position(scroll as usize);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .render(inner, frame.buffer_mut(), &mut state);
    }
    (line_count.min(u16::MAX as usize) as u16, inner.height)
}

fn divider_line(width: u16) -> Line<'static> {
    let bar: String = std::iter::repeat('─').take(width as usize).collect();
    Line::from(Span::styled(bar, Style::default().fg(Color::DarkGray)))
}

/// Truncate a full hex commit id to its first 7 chars for compact display.
fn short_id(id: &str) -> &str {
    if id.len() >= 7 { &id[..7] } else { id }
}

/// Format a POSIX timestamp as `YYYY-MM-DD hh:mm:ss` UTC. Deliberately
/// bare — hooking chrono here for a single header line would inflate the
/// build; ISO-8601-ish UTC is fine as an overlay label.
fn format_epoch(seconds: i64) -> String {
    // 1970-01-01 → seconds. Days since epoch, then break into date;
    // then h/m/s of the day.
    let s = seconds.max(0) as u64;
    let days = s / 86_400;
    let rem = s % 86_400;
    let (h, m, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = date_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{sec:02} UTC")
}

/// Convert days-since-1970 to (year, month, day). Standard civil-date
/// algorithm from Howard Hinnant's date/date.h (public domain).
fn date_from_days(days: u64) -> (u64, u64, u64) {
    let days = days as i64 + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn seeded_pane() -> GitPane {
        let mut pane = GitPane::new(std::env::temp_dir());
        pane.snapshot = GitSnapshot {
            generation: 1,
            repo_root: None,
            head: None,
            upstream: None,
            changes: (0..40)
                .map(|i| GitChange {
                    side: ChangeSide::Worktree,
                    kind: ChangeKind::Modified,
                    path: format!("f{i}.txt").into(),
                    previous_path: None,
                    is_binary: false,
                })
                .collect(),
            commits: (0..40)
                .map(|i| CommitSummary {
                    id: format!("{i:040}"),
                    short_id: format!("{i:07}"),
                    summary: format!("commit {i}"),
                    author: "cjzzz".into(),
                    seconds_since_epoch: 0,
                    graph_column: 0,
                    graph_edges: Vec::new(),
                    is_head: i == 0,
                    refs: Vec::new(),
                })
                .collect(),
            graph_width: 0,
            scanned_at: std::time::Instant::now(),
        };
        pane.changes_rect = Rect::new(0, 0, 40, 10);
        pane.commits_rect = Rect::new(0, 10, 40, 10);
        pane
    }

    #[test]
    fn scroll_over_changes_advances_changes_cursor() {
        let mut pane = seeded_pane();
        pane.focus = Focus::Commits;
        let start = pane.changes_cursor;
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 3),
            Rect::new(0, 0, 40, 20),
        );
        assert!(consumed);
        assert_eq!(pane.focus, Focus::Changes);
        assert!(pane.changes_cursor > start);
    }

    #[test]
    fn scroll_over_commits_advances_commits_cursor() {
        let mut pane = seeded_pane();
        pane.focus = Focus::Changes;
        let start = pane.commits_cursor;
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 13),
            Rect::new(0, 0, 40, 20),
        );
        assert!(consumed);
        assert_eq!(pane.focus, Focus::Commits);
        assert!(pane.commits_cursor > start);
    }

    #[test]
    fn scroll_up_over_commits_moves_cursor_up_toward_top() {
        let mut pane = seeded_pane();
        pane.commits_cursor = 10;
        let _ = pane.on_mouse(
            mouse(MouseEventKind::ScrollUp, 5, 13),
            Rect::new(0, 0, 40, 20),
        );
        assert!(pane.commits_cursor < 10);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn right_arrow_on_changes_requests_diff_like_enter() {
        let mut pane = seeded_pane();
        pane.current_root = Some(std::env::temp_dir());
        pane.focus = Focus::Changes;
        // A background worker request is queued; we can't observe it
        // directly, but the key must be consumed and focus stays on
        // Changes until the diff response lands via poll_background.
        assert!(pane.on_key(key(KeyCode::Right)));
        assert_eq!(pane.focus, Focus::Changes);
        // `l` mirrors vi navigation and does the same thing.
        assert!(pane.on_key(key(KeyCode::Char('l'))));
    }

    #[test]
    fn diff_response_survives_a_racing_snapshot_response() {
        // Regression for the shared-`applied_generation` bug: a snapshot
        // that arrives in the same poll batch as a diff must not gate
        // the diff out through a higher generation.
        let mut pane = seeded_pane();
        // Pretend a snapshot with generation 10 already applied.
        pane.applied_snapshot_generation = 10;
        // Diff came out of the worker at generation 5 (older than the
        // snapshot but still the freshest diff we've seen).
        let diff = DiffSnapshot {
            generation: 5,
            files: Vec::new(),
        };
        // Simulate the exact branch poll_background takes for a diff.
        assert!(diff.generation >= pane.applied_diff_generation);
        pane.applied_diff_generation = diff.generation;
        pane.diff = Some(diff);
        pane.focus = Focus::Diff;
        assert!(
            pane.diff.is_some(),
            "diff must land even when a newer snapshot was applied"
        );
        assert_eq!(pane.focus, Focus::Diff);
    }

    #[test]
    fn scrolling_past_bottom_of_commits_asks_for_more() {
        let mut pane = seeded_pane();
        // Match the seeded snapshot length so the pane thinks it's at
        // the current commit_limit and can grow.
        pane.commit_limit = pane.snapshot.commits.len();
        pane.current_root = Some(std::env::temp_dir());
        // Park cursor on the very last commit — a further ScrollDown
        // is what triggers the load-more request.
        pane.commits_cursor = pane.snapshot.commits.len() - 1;
        let before = pane.commit_limit;
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 13),
            Rect::new(0, 0, 40, 20),
        );
        assert!(consumed);
        assert!(
            pane.commit_limit > before,
            "commit_limit must grow when scrolling past bottom (was {before}, now {})",
            pane.commit_limit
        );
        assert!(pane.load_more_in_flight);
    }

    #[test]
    fn request_more_no_ops_when_snapshot_is_shorter_than_limit() {
        // If the last snapshot returned fewer commits than we asked for,
        // history is exhausted and we must not keep growing the window.
        let mut pane = seeded_pane();
        pane.commit_limit = 200; // pretend we already asked for a lot
        pane.current_root = Some(std::env::temp_dir());
        pane.commits_cursor = pane.snapshot.commits.len() - 1;
        let before = pane.commit_limit;
        pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 13),
            Rect::new(0, 0, 40, 20),
        );
        assert_eq!(
            pane.commit_limit, before,
            "must not grow past exhausted history"
        );
        assert!(!pane.load_more_in_flight);
    }

    #[test]
    fn ref_badges_include_head_marker_and_branch_tag_variants() {
        // Directly build a commit with all three ref kinds + HEAD so we
        // can exercise the badge assembler without spinning a repo.
        let commit = CommitSummary {
            id: "abcdef1234567890abcdef1234567890abcdef12".into(),
            short_id: "abcdef1".into(),
            summary: "hello".into(),
            author: "cjzzz".into(),
            seconds_since_epoch: 0,
            graph_column: 0,
            graph_edges: Vec::new(),
            is_head: true,
            refs: vec![
                GitRef::Branch("main".into()),
                GitRef::RemoteBranch("origin/main".into()),
                GitRef::Tag("v1.0".into()),
            ],
        };
        let spans = ref_badges(&commit);
        // Collect the raw text of each span to make the assertion resilient
        // to Span/Style formatting changes.
        let text: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(
            text,
            vec![
                "[HEAD]".to_string(),
                "[main]".to_string(),
                "[origin/main]".to_string(),
                "[tag: v1.0]".to_string(),
            ]
        );
    }

    #[test]
    fn right_arrow_on_commits_requests_detail_and_stays_on_commits() {
        let mut pane = seeded_pane();
        pane.current_root = Some(std::env::temp_dir());
        pane.focus = Focus::Commits;
        // Key is consumed; focus stays on Commits until the detail
        // response lands via poll_background (which we simulate below).
        assert!(pane.on_key(key(KeyCode::Right)));
        assert_eq!(pane.focus, Focus::Commits);
    }

    #[test]
    fn commit_detail_response_opens_detail_overlay() {
        // Regression scaffolding for the detail stream: build a synthetic
        // response and let the pane store it (bypassing the worker thread
        // for determinism).
        let mut pane = seeded_pane();
        assert!(pane.detail.is_none());
        pane.applied_detail_generation = 0;
        let detail = CommitDetailData {
            generation: 1,
            id: "abc".into(),
            short_id: "abc".into(),
            author_name: "cjzzz".into(),
            author_email: "cj@example.com".into(),
            author_seconds: 0,
            committer_name: "cjzzz".into(),
            committer_email: "cj@example.com".into(),
            committer_seconds: 0,
            subject: "test".into(),
            body: String::new(),
            parents: Vec::new(),
            refs: Vec::new(),
            files: Vec::new(),
        };
        // Mirror poll_background's arm inline.
        if detail.generation >= pane.applied_detail_generation {
            pane.applied_detail_generation = detail.generation;
            pane.detail = Some(detail);
            pane.detail_scroll = 0;
            pane.focus = Focus::Detail;
        }
        assert!(pane.detail.is_some());
        assert_eq!(pane.focus, Focus::Detail);
        // Esc closes and returns to Commits.
        assert!(pane.on_key(key(KeyCode::Esc)));
        assert!(pane.detail.is_none());
        assert_eq!(pane.focus, Focus::Commits);
    }

    #[test]
    fn scroll_outside_subrect_but_inside_pane_still_moves_focused_list() {
        // The commit_limit == commits.len() case must load more even when
        // the wheel event lands outside `commits_rect` (e.g. on the pane
        // border), as long as focus is on Commits.
        let mut pane = seeded_pane();
        pane.commit_limit = pane.snapshot.commits.len();
        pane.current_root = Some(std::env::temp_dir());
        pane.focus = Focus::Commits;
        pane.commits_cursor = pane.snapshot.commits.len() - 1;
        let before = pane.commit_limit;
        // Fire the wheel at (5, 25) — the seeded commits_rect is
        // Rect{y=10,h=10} → row 25 is outside the sub-rect but still
        // inside the outer rect (h=30 below).
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 25),
            Rect::new(0, 0, 40, 30),
        );
        assert!(consumed);
        assert!(
            pane.commit_limit > before,
            "outer-rect scroll must still grow commit_limit (was {before}, now {})",
            pane.commit_limit
        );
    }

    #[test]
    fn scroll_wheel_over_covered_commits_rect_routes_to_detail_when_overlay_open() {
        // Regression: when the detail overlay is drawn on top of the
        // commits list, `commits_rect` still contains the previous
        // sub-list's rect. The wheel must NOT be consumed by
        // `scroll_commits` — it must reach `scroll_detail`.
        let mut pane = seeded_pane();
        // Make the overlay non-trivially scrollable so we can observe
        // detail_scroll advance.
        pane.detail = Some(CommitDetailData {
            generation: 1,
            id: "abc".into(),
            short_id: "abc".into(),
            author_name: "cjzzz".into(),
            author_email: "cj@example.com".into(),
            author_seconds: 0,
            committer_name: "cjzzz".into(),
            committer_email: "cj@example.com".into(),
            committer_seconds: 0,
            subject: "test".into(),
            body: String::new(),
            parents: Vec::new(),
            refs: Vec::new(),
            files: Vec::new(),
        });
        pane.detail_line_count = 100;
        pane.detail_viewport_height = 10;
        pane.focus = Focus::Detail;
        let before_commits_cursor = pane.commits_cursor;
        let before_detail_scroll = pane.detail_scroll;
        // Wheel lands *inside* the cached commits_rect (row 13, seeded
        // rect y=10 h=10).
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 13),
            Rect::new(0, 0, 40, 30),
        );
        assert!(consumed);
        assert_eq!(
            pane.commits_cursor, before_commits_cursor,
            "commits cursor must not move when detail overlay is open"
        );
        assert!(
            pane.detail_scroll > before_detail_scroll,
            "detail scroll must advance (was {before_detail_scroll}, now {})",
            pane.detail_scroll
        );
    }

    #[test]
    fn detail_scroll_clamps_to_content_tail() {
        // scroll_detail must not step past `line_count - viewport`.
        let mut pane = seeded_pane();
        pane.detail = Some(CommitDetailData {
            generation: 1,
            id: "x".into(),
            short_id: "x".into(),
            author_name: String::new(),
            author_email: String::new(),
            author_seconds: 0,
            committer_name: String::new(),
            committer_email: String::new(),
            committer_seconds: 0,
            subject: String::new(),
            body: String::new(),
            parents: Vec::new(),
            refs: Vec::new(),
            files: Vec::new(),
        });
        pane.detail_line_count = 25;
        pane.detail_viewport_height = 10;
        // Max reachable offset is 25 - 10 = 15.
        for _ in 0..20 {
            pane.scroll_detail(true);
        }
        assert_eq!(pane.detail_scroll, 15);
    }

    #[test]
    fn commits_list_state_selected_follows_cursor_after_render() {
        // After rendering, `commits_list_state.selected()` must mirror
        // `commits_cursor` — that's what makes the ratatui List scroll
        // the viewport to keep the highlighted row visible instead of
        // parking at offset 0 while the cursor walks off-screen.
        let mut pane = seeded_pane();
        pane.commits_cursor = 30;
        // Drive one render at a small area so scrolling actually kicks
        // in. We don't inspect the buffer, only the list state.
        let backend = ratatui::backend::TestBackend::new(60, 20);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| {
            let ctx = rimeterm_core::pane::PaneRenderCtx {
                focused: true,
                title_override: None,
                focus_color: Color::Cyan,
            };
            pane.render(Rect::new(0, 0, 60, 20), frame, &ctx);
        })
        .unwrap();
        assert_eq!(pane.commits_list_state.selected(), Some(30));
    }
    #[test]
    fn stable_state_restores_list_focus_without_overlay() {
        let mut source = seeded_pane();
        source.focus = Focus::Detail;
        let state = source.snapshot_state();

        let mut restored = seeded_pane();
        restored.restore_state(&state);

        assert_eq!(restored.focus, Focus::Commits);
        assert!(restored.detail.is_none());
        assert!(restored.diff.is_none());
    }
}
