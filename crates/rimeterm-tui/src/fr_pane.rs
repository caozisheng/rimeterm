use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fast_resume::config::{AGENT_ORDER, AGENTS};
use fast_resume::embed::{
    EmbeddedEngine, ResumeTarget, SearchRequest, SearchResult, resume_target,
};
use fast_resume::model::Session;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use tokio::sync::mpsc::UnboundedSender;
use unicode_width::UnicodeWidthStr;

const RESULT_LIMIT: usize = 100;
const HORIZONTAL_SPLIT_MIN_WIDTH: u16 = 80;
/// Upper bound on `Session.content` bytes we keep in `state.results`.
/// Larger content is silently truncated at a UTF-8 boundary before the
/// preview / cache path sees it. Coding-agent transcripts routinely
/// spill into hundreds of KB; feeding them to `Paragraph::wrap` on
/// every frame is the dominant cost of a busy FR pane on Windows.
/// 64 KiB is enough for tens of screens of scrollable preview without
/// blocking sysmon's 200 ms redraw cadence.
const PREVIEW_CONTENT_MAX: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrAction {
    Resume(ResumeTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitAxis {
    Horizontal,
    Vertical,
}

enum SearchWorkerRequest {
    Search(SearchRequest),
    ReloadAndSearch(SearchRequest),
}

enum WorkerEvent {
    Search(SearchResult),
    Refreshed(Result<usize, String>),
}

struct FrWorker {
    search_tx: Sender<SearchWorkerRequest>,
    refresh_tx: Sender<()>,
    event_rx: Receiver<WorkerEvent>,
}

impl FrWorker {
    fn spawn() -> Self {
        let (search_tx, search_rx) = mpsc::channel();
        let (refresh_tx, refresh_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let search_events = event_tx.clone();
        thread::spawn(move || {
            // Deferred to the worker thread so a slow initial index
            // open (Windows OneDrive-synced HOME, cold NTFS cache)
            // never blocks the tokio runtime worker that runs
            // `FrPane::new`.
            let initial_engine =
                EmbeddedEngine::open_default().map_err(|error| format!("{error:#}"));
            search_worker_loop(search_rx, search_events, initial_engine);
        });
        thread::spawn(move || refresh_worker_loop(refresh_rx, event_tx));
        Self {
            search_tx,
            refresh_tx,
            event_rx,
        }
    }

    fn search(&self, request: SearchRequest) {
        let _ = self.search_tx.send(SearchWorkerRequest::Search(request));
    }

    fn refresh(&self) {
        let _ = self.refresh_tx.send(());
    }

    fn reload_and_search(&self, request: SearchRequest) {
        let _ = self
            .search_tx
            .send(SearchWorkerRequest::ReloadAndSearch(request));
    }
}
fn search_worker_loop(
    request_rx: Receiver<SearchWorkerRequest>,
    event_tx: Sender<WorkerEvent>,
    mut engine: Result<EmbeddedEngine, String>,
) {
    while let Ok(first) = request_rx.recv() {
        let mut request = first;
        let mut reload = matches!(request, SearchWorkerRequest::ReloadAndSearch(_));
        while let Ok(latest) = request_rx.try_recv() {
            reload |= matches!(latest, SearchWorkerRequest::ReloadAndSearch(_));
            request = latest;
        }
        let request = match request {
            SearchWorkerRequest::Search(request)
            | SearchWorkerRequest::ReloadAndSearch(request) => request,
        };
        let reload_error = if reload {
            if engine.is_err() {
                engine = EmbeddedEngine::open_default().map_err(|error| format!("{error:#}"));
                engine.as_ref().err().cloned()
            } else {
                engine
                    .as_mut()
                    .ok()
                    .and_then(|current| current.reload().err())
                    .map(|error| format!("{error:#}"))
            }
        } else {
            None
        };
        let result = if let Some(error) = reload_error {
            SearchResult {
                generation: request.generation,
                sessions: Vec::new(),
                error: Some(error),
            }
        } else {
            match &engine {
                Ok(engine) => engine.search(request),
                Err(error) => SearchResult {
                    generation: request.generation,
                    sessions: Vec::new(),
                    error: Some(error.clone()),
                },
            }
        };
        if event_tx.send(WorkerEvent::Search(result)).is_err() {
            break;
        }
    }
}

fn refresh_worker_loop(request_rx: Receiver<()>, event_tx: Sender<WorkerEvent>) {
    while request_rx.recv().is_ok() {
        while request_rx.try_recv().is_ok() {}
        let refreshed = EmbeddedEngine::open_default()
            .and_then(|mut engine| engine.refresh())
            .map(|summary| summary.sessions)
            .map_err(|error| format!("{error:#}"));
        if event_tx.send(WorkerEvent::Refreshed(refreshed)).is_err() {
            break;
        }
    }
}

#[derive(Debug, Default)]
struct FrState {
    query: String,
    cursor: usize,
    agent_filter: Option<String>,
    requested_generation: u64,
    applied_generation: u64,
    results: Vec<Session>,
    selected: usize,
    preview_scroll: u16,
    status: String,
    /// Directories whose sessions are visually collapsed in the tree.
    collapsed_dirs: HashSet<String>,
}

impl FrState {
    fn handle_key(&mut self, key: KeyEvent) -> Option<FrAction> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('r' | 'R'), modifiers) if modifiers == KeyModifiers::CONTROL => {
                let Some(session) = self.results.get(self.selected) else {
                    self.status = "no session selected".to_string();
                    return None;
                };
                match resume_target(session) {
                    Some(target) => return Some(FrAction::Resume(target)),
                    None => self.status = format!("{} cannot be resumed", session.agent),
                }
            }
            (KeyCode::Char(' '), KeyModifiers::CONTROL) => {
                self.toggle_selected_dir();
            }
            (KeyCode::Up, modifiers) if modifiers.contains(KeyModifiers::ALT) => {
                self.preview_scroll = self.preview_scroll.saturating_sub(3);
            }
            (KeyCode::Down, modifiers) if modifiers.contains(KeyModifiers::ALT) => {
                self.preview_scroll = self.preview_scroll.saturating_add(3);
            }
            (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::PageUp, _) => self.move_selection(-10),
            (KeyCode::PageDown, _) => self.move_selection(10),
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.query.chars().count());
            }
            (KeyCode::Home, _) => self.cursor = 0,
            (KeyCode::End, _) => self.cursor = self.query.chars().count(),
            (KeyCode::Backspace, _) if self.cursor > 0 => {
                let start = char_to_byte_idx(&self.query, self.cursor - 1);
                let end = char_to_byte_idx(&self.query, self.cursor);
                self.query.replace_range(start..end, "");
                self.cursor -= 1;
                self.request_search();
            }
            (KeyCode::Delete, _) if self.cursor < self.query.chars().count() => {
                let start = char_to_byte_idx(&self.query, self.cursor);
                let end = char_to_byte_idx(&self.query, self.cursor + 1);
                self.query.replace_range(start..end, "");
                self.request_search();
            }
            (KeyCode::Tab, _) => {
                self.cycle_agent(false);
                self.request_search();
            }
            (KeyCode::BackTab, _) => {
                self.cycle_agent(true);
                self.request_search();
            }
            (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                let byte = char_to_byte_idx(&self.query, self.cursor);
                self.query.insert(byte, ch);
                self.cursor += 1;
                self.request_search();
            }
            _ => {}
        }
        None
    }

    fn request_search(&mut self) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.status = "searching...".to_string();
    }

    fn search_request(&self) -> SearchRequest {
        SearchRequest {
            generation: self.requested_generation,
            query: self.query.clone(),
            agent_filter: self.agent_filter.clone(),
            limit: RESULT_LIMIT,
        }
    }

    fn apply_result(&mut self, result: SearchResult) -> bool {
        if result.generation != self.requested_generation {
            return false;
        }
        self.applied_generation = result.generation;
        if let Some(error) = result.error {
            self.status = format!("search failed: {error}");
            return true;
        }
        // Bound each session's `content` before it enters `results` so
        // the preview / cache paths never wrap megabytes of transcript
        // per redraw. Truncation keeps the leading portion (users read
        // top-to-bottom and can Alt+↑↓ scroll within it).
        self.results = result
            .sessions
            .into_iter()
            .map(|mut session| {
                session.content = truncate_utf8_bytes(session.content, PREVIEW_CONTENT_MAX);
                session
            })
            .collect();
        group_by_directory(&mut self.results);
        self.selected = 0;
        self.preview_scroll = 0;
        self.status = format!("{} sessions", self.results.len());
        true
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.results.len() as isize - 1;
        let step = if delta > 0 { 1isize } else { -1 };
        let mut remaining = delta.unsigned_abs();
        let mut pos = self.selected as isize;
        while remaining > 0 {
            pos += step;
            if pos < 0 || pos > last {
                pos = pos.clamp(0, last);
                break;
            }
            if !self.is_collapsed(pos as usize) {
                remaining -= 1;
            }
        }
        // If we landed on a collapsed session, scan forward to the next visible one.
        while pos <= last && self.is_collapsed(pos as usize) {
            pos += step;
        }
        if pos < 0 || pos > last || self.is_collapsed(pos as usize) {
            // Scan the other direction.
            pos = self.selected as isize;
        }
        self.selected = pos.clamp(0, last) as usize;
        self.preview_scroll = 0;
    }

    /// Returns true if session at `idx` belongs to a collapsed directory.
    fn is_collapsed(&self, idx: usize) -> bool {
        self.results
            .get(idx)
            .is_some_and(|s| self.collapsed_dirs.contains(&s.directory))
    }

    /// Toggle collapse for the directory of the currently selected session.
    fn toggle_selected_dir(&mut self) {
        let Some(session) = self.results.get(self.selected) else {
            return;
        };
        let dir = session.directory.clone();
        if self.collapsed_dirs.contains(&dir) {
            self.collapsed_dirs.remove(&dir);
        } else {
            self.collapsed_dirs.insert(dir);
            // If selected is now hidden, find next visible.
            self.ensure_selection_visible();
        }
    }

    /// Move selection to the nearest visible (non-collapsed) session.
    fn ensure_selection_visible(&mut self) {
        if self.results.is_empty() {
            return;
        }
        if !self.is_collapsed(self.selected) {
            return;
        }
        // Search forward first.
        for i in self.selected + 1..self.results.len() {
            if !self.is_collapsed(i) {
                self.selected = i;
                self.preview_scroll = 0;
                return;
            }
        }
        // Search backward.
        for i in (0..self.selected).rev() {
            if !self.is_collapsed(i) {
                self.selected = i;
                self.preview_scroll = 0;
                return;
            }
        }
    }

    fn cycle_agent(&mut self, reverse: bool) {
        let current = self
            .agent_filter
            .as_deref()
            .and_then(|agent| AGENT_ORDER.iter().position(|candidate| *candidate == agent));
        self.agent_filter = if reverse {
            match current {
                None => AGENT_ORDER.last().map(|agent| (*agent).to_string()),
                Some(0) => None,
                Some(index) => Some(AGENT_ORDER[index - 1].to_string()),
            }
        } else {
            match current {
                None => Some(AGENT_ORDER[0].to_string()),
                Some(index) if index + 1 < AGENT_ORDER.len() => {
                    Some(AGENT_ORDER[index + 1].to_string())
                }
                Some(_) => None,
            }
        };
    }

    fn split_axis(width: u16) -> SplitAxis {
        if width >= HORIZONTAL_SPLIT_MIN_WIDTH {
            SplitAxis::Horizontal
        } else {
            SplitAxis::Vertical
        }
    }
}

pub struct FrPane {
    id: PaneId,
    title: String,
    state: FrState,
    worker: FrWorker,
    action_tx: UnboundedSender<FrAction>,
    results_rect: Rect,
    preview_rect: Rect,
    results_offset: usize,
    /// Cached preview buffer keyed on the state that changes what the
    /// preview paints. When the key matches the caller's rect + state,
    /// `render` blits `buf` into the frame instead of re-running
    /// `Paragraph::wrap` over the (potentially 64 KiB) session content.
    /// See `PREVIEW_CONTENT_MAX` for the sizing rationale.
    preview_cache: Option<PreviewCache>,
}

/// Snapshot of the FR preview area. `key` covers every input that
/// changes the painted cells; `buf` is sized to the cached rect
/// (`x=0, y=0, w=key.width, h=key.height`) so it can be blitted at any
/// origin later.
struct PreviewCache {
    key: PreviewCacheKey,
    buf: Buffer,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PreviewCacheKey {
    /// Generation of the search result the preview reflects. Bumps when
    /// `apply_result` swaps in a new `results` vec.
    result_generation: u64,
    selected: usize,
    preview_scroll: u16,
    /// Rect dimensions only — the origin doesn't affect wrap layout.
    width: u16,
    height: u16,
}

impl FrPane {
    pub fn new(action_tx: UnboundedSender<FrAction>) -> Self {
        let worker = FrWorker::spawn();
        let mut state = FrState::default();
        state.request_search();
        state.status = "loading index...".to_string();
        let request = state.search_request();
        worker.search(request);
        worker.refresh();
        Self {
            id: PaneId::next(),
            title: "sessions".to_string(),
            state,
            worker,
            action_tx,
            results_rect: Rect::default(),
            preview_rect: Rect::default(),
            results_offset: 0,
            preview_cache: None,
        }
    }

    fn send_search(&self) {
        self.worker.search(self.state.search_request());
    }

    /// Paint the preview panel, reusing a cached `Buffer` whenever the
    /// inputs that affect the wrap layout haven't changed.
    ///
    /// Why: `Paragraph::new(session.content).wrap(...)` re-wraps the
    /// entire content on every frame. sysmon drives a global redraw
    /// every 200 ms; letting FR redo an O(content) wrap on top of that
    /// is the leading cause of Windows-side slowdown as the user
    /// browses long transcripts. The cache reduces the steady-state
    /// cost to `w*h` cell clones (one memcpy-ish blit).
    fn render_preview_cached(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if area.width == 0 || area.height == 0 {
            self.preview_cache = None;
            return;
        }
        let key = PreviewCacheKey {
            result_generation: self.state.applied_generation,
            selected: self.state.selected,
            preview_scroll: self.state.preview_scroll,
            width: area.width,
            height: area.height,
        };
        let hit = matches!(&self.preview_cache, Some(cache) if cache.key == key);
        if !hit {
            let cache_area = Rect::new(0, 0, area.width, area.height);
            let mut scratch = Buffer::empty(cache_area);
            render_preview_into(&mut scratch, cache_area, &self.state);
            self.preview_cache = Some(PreviewCache { key, buf: scratch });
        }
        if let Some(cache) = &self.preview_cache {
            blit_buffer(&cache.buf, frame.buffer_mut(), (area.x, area.y));
        }
    }

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let mut values = std::collections::BTreeMap::new();
        values.insert("query".into(), self.state.query.clone());
        if let Some(agent) = &self.state.agent_filter {
            values.insert("agent".into(), agent.clone());
        }
        rimeterm_config::memory_state::PaneState { values }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        self.state.query = state.values.get("query").cloned().unwrap_or_default();
        self.state.cursor = self.state.query.chars().count();
        self.state.agent_filter = state
            .values
            .get("agent")
            .filter(|agent| AGENT_ORDER.contains(&agent.as_str()))
            .cloned();
        self.state.request_search();
        self.send_search();
    }
}

impl PaneProvider for FrPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut ratatui::Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let border_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(" Fast Resume ");
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        if inner.width == 0 || inner.height == 0 {
            return RenderOutcome::default();
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);
        render_search(frame, rows[0], &self.state);

        let body = match FrState::split_axis(rows[1].width) {
            SplitAxis::Horizontal => Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(rows[1]),
            SplitAxis::Vertical => Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(rows[1]),
        };
        self.results_rect = body[0];
        self.preview_rect = body[1];
        self.results_offset = render_results(frame, body[0], &self.state);
        self.render_preview_cached(frame, body[1]);
        render_footer(frame, rows[2], &self.state);

        let search_inner = Block::default().borders(Borders::ALL).inner(rows[0]);
        let prefix_width = 2u16;
        let byte = char_to_byte_idx(&self.state.query, self.state.cursor);
        let cursor_width = UnicodeWidthStr::width(&self.state.query[..byte]) as u16;
        let cursor = ctx.focused.then_some((
            search_inner
                .x
                .saturating_add(prefix_width)
                .saturating_add(cursor_width)
                .min(search_inner.right().saturating_sub(1)),
            search_inner.y,
        ));
        RenderOutcome {
            request_redraw: false,
            cursor,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        let before_generation = self.state.requested_generation;
        if let Some(action) = self.state.handle_key(key) {
            let _ = self.action_tx.send(action);
        }
        if self.state.requested_generation != before_generation {
            self.send_search();
        }
        fr_key_consumed(key)
    }

    fn on_mouse(&mut self, event: MouseEvent, _outer_rect: Rect) -> bool {
        let point_in = |rect: Rect| {
            event.column >= rect.x
                && event.column < rect.right()
                && event.row >= rect.y
                && event.row < rect.bottom()
        };
        match event.kind {
            MouseEventKind::ScrollUp if point_in(self.preview_rect) => {
                self.state.preview_scroll = self.state.preview_scroll.saturating_sub(3);
                true
            }
            MouseEventKind::ScrollDown if point_in(self.preview_rect) => {
                self.state.preview_scroll = self.state.preview_scroll.saturating_add(3);
                true
            }
            MouseEventKind::ScrollUp if point_in(self.results_rect) => {
                self.state.move_selection(-3);
                true
            }
            MouseEventKind::ScrollDown if point_in(self.results_rect) => {
                self.state.move_selection(3);
                true
            }
            MouseEventKind::Down(MouseButton::Left) if point_in(self.results_rect) => {
                let click_row = event
                    .row
                    .saturating_sub(self.results_rect.y.saturating_add(1))
                    as usize;
                let display = build_display_rows(&self.state.results, &self.state.collapsed_dirs);
                let abs_row = self.results_offset.saturating_add(click_row);
                match display.get(abs_row) {
                    Some(DisplayRow::Header { dir, .. }) => {
                        let dir = dir.clone();
                        if self.state.collapsed_dirs.contains(&dir) {
                            self.state.collapsed_dirs.remove(&dir);
                        } else {
                            self.state.collapsed_dirs.insert(dir);
                            self.state.ensure_selection_visible();
                        }
                    }
                    Some(DisplayRow::Session(idx)) => {
                        self.state.selected = *idx;
                        self.state.preview_scroll = 0;
                    }
                    None => {}
                }
                true
            }
            _ => false,
        }
    }

    fn poll_background(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(event) = self.worker.event_rx.try_recv() {
            match event {
                WorkerEvent::Search(result) => dirty |= self.state.apply_result(result),
                WorkerEvent::Refreshed(Ok(total)) => {
                    self.state.status = format!("index refreshed: {total} sessions");
                    self.worker.reload_and_search(self.state.search_request());
                    dirty = true;
                }
                WorkerEvent::Refreshed(Err(error)) => {
                    self.state.status = format!("refresh failed: {error}");
                    dirty = true;
                }
            }
        }
        dirty
    }

    fn reload(&mut self) {
        self.state.request_search();
        self.state.status = "refreshing index...".to_string();
        self.worker.refresh();
    }
}

fn char_to_byte_idx(value: &str, char_idx: usize) -> usize {
    value
        .char_indices()
        .nth(char_idx)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

/// Truncate `value` to at most `max_bytes` bytes at a valid UTF-8
/// boundary. Cheap when the input is already short. `String` (not
/// `&str`) so the common "no truncation" path returns without an
/// alloc / copy.
fn truncate_utf8_bytes(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    // Walk backwards from `max_bytes` to the previous char boundary
    // (str::is_char_boundary is O(1)). `max_bytes` may land inside a
    // multi-byte sequence; the loop backs off at most 3 bytes.
    let mut cut = max_bytes;
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = value;
    truncated.truncate(cut);
    truncated
}

fn fr_key_consumed(key: KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Char(_)
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Tab
            | KeyCode::BackTab
    )
}

fn render_search(frame: &mut ratatui::Frame<'_>, area: Rect, state: &FrState) {
    let filter = state.agent_filter.as_deref().unwrap_or("all");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Search [{filter}] "));
    let line = if state.query.is_empty() {
        Line::from(vec![
            Span::raw("/ "),
            Span::styled(
                "search titles, messages, paths",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![Span::raw("/ "), Span::raw(state.query.as_str())])
    };
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_results(frame: &mut ratatui::Frame<'_>, area: Rect, state: &FrState) -> usize {
    let block = Block::default().borders(Borders::ALL).title(" Results ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return 0;
    }

    if state.results.is_empty() {
        frame.render_widget(
            Paragraph::new("  No sessions found").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return 0;
    }

    let display = build_display_rows(&state.results, &state.collapsed_dirs);
    let total = display.len();

    // Find the display row for the selected session.
    let selected_display = display
        .iter()
        .position(|row| matches!(row, DisplayRow::Session(idx) if *idx == state.selected))
        .unwrap_or(0);

    let max_rows = inner.height as usize;
    if max_rows == 0 {
        return 0;
    }
    let start = selected_display
        .saturating_sub(max_rows.saturating_sub(1))
        .min(total.saturating_sub(1));
    let end = (start + max_rows).min(total);

    for (screen_row, display_row) in display[start..end].iter().enumerate() {
        let y = inner.y + screen_row as u16;
        match display_row {
            DisplayRow::Header {
                name,
                count,
                collapsed,
                ..
            } => {
                let arrow = if *collapsed { "  ▸ " } else { "  ▾ " };
                let count_str = format!(" ({})", count);
                let max_name = (inner.width as usize).saturating_sub(4 + count_str.len());
                let label = truncate_display(name, max_name);
                let line = Line::from(vec![
                    Span::styled(arrow, Style::default().fg(DIR_HEADER_FG).bold()),
                    Span::styled(label, Style::default().fg(DIR_HEADER_FG).bold()),
                    Span::styled(count_str, Style::default().fg(Color::DarkGray)),
                ]);
                frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
            }
            DisplayRow::Session(idx) => {
                let session = &state.results[*idx];
                let selected = *idx == state.selected;
                render_session_row(frame, inner, y, session, selected);
            }
        }
    }
    start
}

const DIR_HEADER_FG: Color = Color::Rgb(140, 160, 180);

enum DisplayRow {
    Header {
        name: String,
        count: usize,
        dir: String,
        collapsed: bool,
    },
    Session(usize),
}

fn build_display_rows(sessions: &[Session], collapsed: &HashSet<String>) -> Vec<DisplayRow> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < sessions.len() {
        let dir = &sessions[i].directory;
        let group_start = i;
        while i < sessions.len() && sessions[i].directory == *dir {
            i += 1;
        }
        let is_collapsed = collapsed.contains(dir.as_str());
        rows.push(DisplayRow::Header {
            name: sessions[group_start].workspace_name().to_string(),
            count: i - group_start,
            dir: dir.clone(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            for idx in group_start..i {
                rows.push(DisplayRow::Session(idx));
            }
        }
    }
    rows
}

fn render_session_row(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    y: u16,
    session: &Session,
    selected: bool,
) {
    let row_style = if selected {
        Style::default().bg(Color::Rgb(68, 52, 34)).fg(Color::White)
    } else {
        Style::default()
    };

    // Fill background.
    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(row_style),
        Rect::new(area.x, y, area.width, 1),
    );

    let agent_config = AGENTS.get(session.agent.as_str());
    let agent_color = agent_config.map(|a| a.color).unwrap_or(Color::White);
    let agent_label = agent_config
        .map(|a| a.badge)
        .unwrap_or(session.agent.as_str());
    let age_text = fast_resume::tui::text::time_ago(session.timestamp);
    let pointer = if selected { "› " } else { "  " };

    // "    › agent  3h  title..."
    let indent: u16 = 4;
    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut used: u16 = indent;

    // Pointer.
    spans.push(Span::styled(
        format!("{:>w$}", pointer, w = indent as usize),
        row_style,
    ));

    // Agent badge (colored, bold).
    let aw = (agent_label.width() as u16).min(area.width.saturating_sub(used));
    spans.push(Span::styled(
        truncate_display(agent_label, aw as usize),
        row_style.fg(agent_color).add_modifier(Modifier::BOLD),
    ));
    used += aw;

    // Gap + age (gray).
    if used < area.width {
        spans.push(Span::styled(" ", row_style));
        used += 1;
        let age_w = (age_text.width() as u16).min(area.width.saturating_sub(used));
        spans.push(Span::styled(
            truncate_display(&age_text, age_w as usize),
            row_style.fg(Color::DarkGray),
        ));
        used += age_w;
    }

    // Gap + title.
    if used < area.width {
        spans.push(Span::styled(" ", row_style));
        used += 1;
        let title_w = area.width.saturating_sub(used) as usize;
        spans.push(Span::styled(
            truncate_display(&session.title, title_w),
            row_style,
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, y, area.width, 1),
    );
}

/// Truncate a display string to `max_width` columns, appending "..." if needed.
fn truncate_display(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }
    let keep = max_width.saturating_sub(3);
    let mut out = String::new();
    for ch in value.chars() {
        if UnicodeWidthStr::width(out.as_str())
            + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
            > keep
        {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

/// Reorder sessions so they are clustered by directory.
/// Groups are ordered by first appearance (= most recent session).
fn group_by_directory(sessions: &mut Vec<Session>) {
    if sessions.len() <= 1 {
        return;
    }
    let mut dir_rank = std::collections::HashMap::<String, usize>::new();
    for session in sessions.iter() {
        let len = dir_rank.len();
        dir_rank.entry(session.directory.clone()).or_insert(len);
    }
    sessions.sort_by_key(|s| dir_rank.get(&s.directory).copied().unwrap_or(usize::MAX));
}

fn render_preview_into(buf: &mut Buffer, area: Rect, state: &FrState) {
    let Some(session) = state.results.get(state.selected) else {
        Paragraph::new("No matching sessions")
            .block(Block::default().borders(Borders::ALL).title(" Preview "))
            .render(area, buf);
        return;
    };
    let title = format!(" {} · {} ", session.agent, session.title);
    Paragraph::new(session.content.as_str())
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((state.preview_scroll, 0))
        .render(area, buf);
}

/// Copy every cell of `src` into `dst` starting at `dst_origin`.
/// Mirrors `pty_pane::blit_buffer_at` — kept local because the two
/// modules live in the same crate but neither exports a helper the
/// other can depend on without introducing a cycle in the pub API.
fn blit_buffer(src: &Buffer, dst: &mut Buffer, dst_origin: (u16, u16)) {
    let (dx, dy) = dst_origin;
    let src_area = src.area();
    for y in 0..src_area.height {
        for x in 0..src_area.width {
            dst[(dx + x, dy + y)] = src[(x, y)].clone();
        }
    }
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, state: &FrState) {
    let hints = "↑↓ select  C-Space fold  Alt+↑↓ preview  Tab agent  C-R resume";
    let width = area.width as usize;
    let status_width = UnicodeWidthStr::width(state.status.as_str());
    let hint_width = UnicodeWidthStr::width(hints);
    let line = if hint_width + status_width + 2 <= width {
        format!("{hints}  {}", state.status)
    } else if status_width <= width {
        state.status.clone()
    } else {
        state.status.chars().take(width).collect()
    };
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}
#[cfg(test)]
mod tests {
    use chrono::Local;
    use std::path::PathBuf;

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn session(id: &str) -> Session {
        Session::new(
            id,
            "codex",
            format!("Session {id}"),
            "C:/work",
            Local::now(),
            "» prompt\n\n  response",
            2,
        )
    }

    #[test]
    fn printable_key_inserts_at_query_cursor() {
        let mut state = FrState {
            query: "ac".to_string(),
            cursor: 1,
            ..FrState::default()
        };

        state.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE));

        assert_eq!(state.query, "abc");
    }

    #[test]
    fn stale_search_result_is_rejected() {
        let mut state = FrState {
            requested_generation: 3,
            ..FrState::default()
        };

        let applied = state.apply_result(SearchResult {
            generation: 2,
            sessions: vec![session("stale")],
            error: None,
        });

        assert!(!applied);
    }

    #[test]
    fn latest_search_result_resets_selection_and_preview_scroll() {
        let mut state = FrState {
            requested_generation: 4,
            selected: 9,
            preview_scroll: 12,
            ..FrState::default()
        };

        state.apply_result(SearchResult {
            generation: 4,
            sessions: vec![session("fresh")],
            error: None,
        });

        assert_eq!((state.selected, state.preview_scroll), (0, 0));
    }

    #[test]
    fn ctrl_r_emits_resume_action_for_selected_session() {
        let mut state = FrState {
            results: vec![session("42")],
            ..FrState::default()
        };

        let action = state.handle_key(key(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert_eq!(
            action,
            Some(FrAction::Resume(ResumeTarget {
                agent: "codex".to_string(),
                argv: vec!["codex".to_string(), "resume".to_string(), "42".to_string(),],
                cwd: PathBuf::from("C:/work"),
            }))
        );
    }

    #[test]
    fn narrow_pane_stacks_results_over_preview() {
        assert_eq!(FrState::split_axis(48), SplitAxis::Vertical);
    }

    #[test]
    fn wide_pane_places_results_beside_preview() {
        assert_eq!(FrState::split_axis(100), SplitAxis::Horizontal);
    }
    #[test]
    fn stable_state_restores_query_and_agent_filter() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut source = FrPane::new(tx.clone());
        source.state.query = "resume work".into();
        source.state.agent_filter = Some(AGENT_ORDER[0].into());
        let state = source.snapshot_state();

        let mut restored = FrPane::new(tx);
        restored.restore_state(&state);

        assert_eq!(restored.state.query, "resume work");
        assert_eq!(restored.state.agent_filter.as_deref(), Some(AGENT_ORDER[0]));
        assert_eq!(restored.state.cursor, "resume work".chars().count());
    }
}
