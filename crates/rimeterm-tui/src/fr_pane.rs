use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fast_resume::config::AGENT_ORDER;
use fast_resume::embed::{
    EmbeddedEngine, ResumeTarget, SearchRequest, SearchResult, resume_target,
};
use fast_resume::model::Session;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use tokio::sync::mpsc::UnboundedSender;
use unicode_width::UnicodeWidthStr;

const RESULT_LIMIT: usize = 100;
const HORIZONTAL_SPLIT_MIN_WIDTH: u16 = 80;

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
        let initial_engine = EmbeddedEngine::open_default().map_err(|error| format!("{error:#}"));
        thread::spawn(move || search_worker_loop(search_rx, search_events, initial_engine));
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
        self.results = result.sessions;
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
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.preview_scroll = 0;
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
            title: "FR".to_string(),
            state,
            worker,
            action_tx,
            results_rect: Rect::default(),
            preview_rect: Rect::default(),
            results_offset: 0,
        }
    }

    fn send_search(&self) {
        self.worker.search(self.state.search_request());
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
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(rows[1]),
            SplitAxis::Vertical => Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(rows[1]),
        };
        self.results_rect = body[0];
        self.preview_rect = body[1];
        self.results_offset = render_results(frame, body[0], &self.state);
        render_preview(frame, body[1], &self.state);
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
                let row = event
                    .row
                    .saturating_sub(self.results_rect.y.saturating_add(1))
                    as usize;
                let selected = self.results_offset.saturating_add(row);
                if selected < self.state.results.len() {
                    self.state.selected = selected;
                    self.state.preview_scroll = 0;
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
    let items = state.results.iter().map(|session| {
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("{} ", session.agent),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(session.title.as_str()),
        ]))
    });
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Results "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    let visible = area.height.saturating_sub(2) as usize;
    let offset = if visible == 0 {
        0
    } else {
        state.selected.saturating_add(1).saturating_sub(visible)
    };
    let mut list_state =
        ListState::default().with_selected((!state.results.is_empty()).then_some(state.selected));
    *list_state.offset_mut() = offset;
    frame.render_stateful_widget(list, area, &mut list_state);
    offset
}

fn render_preview(frame: &mut ratatui::Frame<'_>, area: Rect, state: &FrState) {
    let Some(session) = state.results.get(state.selected) else {
        frame.render_widget(
            Paragraph::new("No matching sessions")
                .block(Block::default().borders(Borders::ALL).title(" Preview ")),
            area,
        );
        return;
    };
    let title = format!(" {} · {} ", session.agent, session.title);
    frame.render_widget(
        Paragraph::new(session.content.as_str())
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((state.preview_scroll, 0)),
        area,
    );
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, state: &FrState) {
    let hints = "↑↓ select  Alt+↑↓ preview  Tab agent  Ctrl+R resume";
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
}
