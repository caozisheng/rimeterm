//! Native stock watchlist, search, and detail pane.
//!
//! Three columns render side by side (A股 / 港股 / 美股), each with its own
//! cursor, viewport offset, scrollbar, and refresh cadence. `Tab`/`Shift+Tab`
//! cycle the focused column; the mouse wheel scrolls (and focuses) the column
//! under the pointer. Narrow terminals collapse to a single column that still
//! honours the focused column's independent state.

use std::any::Any;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Widget, Wrap,
    },
};
use rimeterm_config::StockConfig;
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_stock::{
    DetailBundle, IndexRow, Market, NewsItem, QuoteRow, SearchResult, Snapshot, WatchEntry,
    WatchList, format_change_pct, format_compact, format_price,
};

use crate::stock_model::{StockRequest, StockResponse};
use crate::stock_worker::StockWorker;

const WHEEL_STEP: i32 = 3;
const SEARCH_LIMIT: usize = 20;
const HIDDEN_REFRESH: Duration = Duration::from_secs(60);
/// Minimum terminal width for the three-column side-by-side layout. Below this
/// only the focused column renders — the other two keep their own state so
/// widening the terminal restores them where the user left off.
const MIN_MULTI_COLUMN_WIDTH: u16 = 90;

#[derive(Clone, Debug)]
enum Modal {
    None,
    Search {
        input: String,
        results: Vec<SearchResult>,
        cursor: usize,
        requested_input: Option<String>,
        dirty_since: Option<Instant>,
    },
    ConfirmDelete(WatchEntry),
}

#[derive(Clone, Debug)]
enum View {
    List,
    Detail {
        entry: WatchEntry,
        bundle: Option<DetailBundle>,
        news_cursor: usize,
    },
}

/// Per-market independent view state. Each column keeps its cursor, scroll
/// offset, cached body rect for hit-testing, refresh clock, and fetching flag.
#[derive(Clone, Debug)]
struct ColumnState {
    market: Market,
    rows: Vec<QuoteRow>,
    indices: Vec<IndexRow>,
    cursor: usize,
    /// Top-of-viewport row index. Advanced by page/wheel scrolling so the
    /// scrollbar thumb tracks the visible window rather than the cursor row.
    scroll: usize,
    body_rect: Rect,
    fetching: bool,
    refresh_generation: u64,
    request_started: Instant,
    next_refresh: Instant,
    last_update: Option<Instant>,
    last_error: Option<String>,
}

impl ColumnState {
    fn new(market: Market) -> Self {
        Self {
            market,
            rows: Vec::new(),
            indices: Vec::new(),
            cursor: 0,
            scroll: 0,
            body_rect: Rect::default(),
            fetching: false,
            refresh_generation: 0,
            request_started: Instant::now(),
            next_refresh: Instant::now(),
            last_update: None,
            last_error: None,
        }
    }

    fn ensure_cursor_visible(&mut self, viewport: usize) {
        if self.rows.is_empty() || viewport == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let max = self.rows.len() - 1;
        if self.cursor > max {
            self.cursor = max;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + viewport {
            self.scroll = self.cursor + 1 - viewport;
        }
        let max_scroll = self.rows.len().saturating_sub(viewport);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.rows.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let max = self.rows.len() - 1;
        self.cursor = (self.cursor as i32)
            .saturating_add(delta)
            .clamp(0, max as i32) as usize;
    }

    fn scroll_by(&mut self, delta: i32) {
        if self.rows.is_empty() {
            self.scroll = 0;
            return;
        }
        let viewport = self.body_rect.height.saturating_sub(1) as usize;
        let max_scroll = self.rows.len().saturating_sub(viewport.max(1));
        let next = (self.scroll as i32).saturating_add(delta).max(0) as usize;
        self.scroll = next.min(max_scroll);
        // Wheel scroll also pulls the cursor into the visible window so a
        // subsequent Enter operates on something the user can see.
        if viewport > 0 {
            if self.cursor < self.scroll {
                self.cursor = self.scroll;
            } else if self.cursor >= self.scroll + viewport {
                self.cursor = self.scroll + viewport - 1;
            }
        }
    }
}

pub struct StockPane {
    id: PaneId,
    worker: StockWorker,
    config: StockConfig,
    watchlist_path: PathBuf,
    watchlist: WatchList,
    /// Column state for A股 / 港股 / 美股, always in this order.
    columns: [ColumnState; 3],
    focused: usize,
    modal: Modal,
    view: View,
    visible: bool,
    detail_fetching: bool,
    next_generation: u64,
    search_generation: u64,
    detail_generation: u64,
    hint: Option<String>,
}

impl StockPane {
    pub fn new(config: StockConfig, watchlist_path: PathBuf) -> Self {
        let watchlist =
            WatchList::load_or_seed(&watchlist_path).unwrap_or_else(|_| WatchList::seeded());
        let worker = StockWorker::spawn(config.http_proxy.clone(), config.tushare_token.clone());
        let mut pane = Self {
            id: PaneId::next(),
            worker,
            config,
            watchlist_path,
            watchlist,
            columns: [
                ColumnState::new(Market::AShare),
                ColumnState::new(Market::HongKong),
                ColumnState::new(Market::Us),
            ],
            focused: 0,
            modal: Modal::None,
            view: View::List,
            visible: false,
            detail_fetching: false,
            next_generation: 0,
            search_generation: 0,
            detail_generation: 0,
            hint: None,
        };
        for column_index in 0..pane.columns.len() {
            pane.request_column_refresh(column_index);
        }
        pane
    }

    fn focused_market(&self) -> Market {
        self.columns[self.focused].market
    }

    fn column_index(&self, market: Market) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.market == market)
    }

    fn request_column_refresh(&mut self, column_index: usize) {
        let column = &mut self.columns[column_index];
        if column.fetching {
            return;
        }
        self.next_generation = self.next_generation.saturating_add(1);
        column.refresh_generation = self.next_generation;
        column.fetching = true;
        column.request_started = Instant::now();
        let market = column.market;
        let watchlist = self.watchlist.entries.clone();
        self.worker.send(StockRequest::Refresh {
            generation: self.next_generation,
            market,
            watchlist,
        });
    }

    fn request_all_refresh(&mut self, force: bool) {
        for column_index in 0..self.columns.len() {
            if force {
                self.columns[column_index].fetching = false;
            }
            self.request_column_refresh(column_index);
        }
    }

    fn request_detail(&mut self, entry: WatchEntry) {
        self.next_generation = self.next_generation.saturating_add(1);
        self.detail_generation = self.next_generation;
        self.worker.send(StockRequest::Detail {
            generation: self.detail_generation,
            entry,
        });
        self.detail_fetching = true;
    }

    fn request_search(&mut self, input: String) {
        self.next_generation = self.next_generation.saturating_add(1);
        self.search_generation = self.next_generation;
        self.worker.send(StockRequest::Search {
            generation: self.search_generation,
            market: self.focused_market(),
            query: input,
            limit: SEARCH_LIMIT,
        });
    }

    fn request_live_detail(&mut self, entry: WatchEntry) {
        self.next_generation = self.next_generation.saturating_add(1);
        self.detail_generation = self.next_generation;
        self.worker.send(StockRequest::LiveDetail {
            generation: self.detail_generation,
            entry,
        });
        self.detail_fetching = true;
    }

    fn selected_entry(&self) -> Option<WatchEntry> {
        let column = &self.columns[self.focused];
        column.rows.get(column.cursor).map(|row| WatchEntry {
            market: row.market,
            symbol: row.symbol.clone(),
            name: row.name.clone(),
        })
    }

    fn focus_market(&mut self, market: Market) {
        if let Some(index) = self.column_index(market) {
            self.focused = index;
        }
    }

    fn cycle_focus(&mut self, reverse: bool) {
        let len = self.columns.len();
        self.focused = if reverse {
            (self.focused + len - 1) % len
        } else {
            (self.focused + 1) % len
        };
    }

    fn move_cursor(&mut self, delta: i32) {
        self.columns[self.focused].move_cursor(delta);
    }

    fn open_detail(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        self.view = View::Detail {
            entry: entry.clone(),
            bundle: None,
            news_cursor: 0,
        };
        self.request_detail(entry);
    }

    fn save_watchlist(&mut self) {
        match self.watchlist.save_atomic(&self.watchlist_path) {
            Ok(()) => self.request_all_refresh(true),
            Err(error) => self.hint = Some(format!("watchlist: {error}")),
        }
    }

    fn refresh_interval(&self, market: Market) -> Duration {
        if !self.visible {
            return HIDDEN_REFRESH.max(Duration::from_secs(self.config.closed_refresh_secs.max(1)));
        }
        market.refresh_interval(
            Utc::now(),
            self.config.open_refresh_hz,
            self.config.closed_refresh_secs,
        )
    }

    fn on_search_key(&mut self, key: KeyEvent) -> bool {
        let mut query = None;
        let mut add = None;
        match &mut self.modal {
            Modal::Search {
                input,
                results,
                cursor,
                dirty_since,
                ..
            } => match key.code {
                KeyCode::Char(c) => {
                    input.push(c);
                    *cursor = 0;
                    *dirty_since = Some(Instant::now());
                }
                KeyCode::Backspace => {
                    input.pop();
                    *cursor = 0;
                    *dirty_since = Some(Instant::now());
                }
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => {
                    if !results.is_empty() {
                        *cursor = (*cursor + 1).min(results.len() - 1);
                    }
                }
                KeyCode::Enter => add = results.get(*cursor).cloned(),
                KeyCode::Tab => {
                    query = Some(input.clone());
                }
                KeyCode::Esc => self.modal = Modal::None,
                _ => return false,
            },
            _ => return false,
        }
        if let Some(search) = add {
            let entry = WatchEntry {
                market: search.market,
                symbol: search.symbol,
                name: search.name,
            };
            let market = entry.market;
            if self.watchlist.add(entry) {
                self.save_watchlist();
                self.focus_market(market);
                self.hint = Some("added to watchlist".into());
            } else {
                self.hint = Some("already in watchlist".into());
            }
            self.modal = Modal::None;
        }
        if let Some(query) = query {
            self.cycle_focus(key.modifiers.contains(KeyModifiers::SHIFT));
            if let Modal::Search {
                input,
                results,
                cursor,
                requested_input,
                dirty_since,
            } = &mut self.modal
            {
                *input = query;
                results.clear();
                *cursor = 0;
                *requested_input = None;
                *dirty_since = Some(Instant::now());
            }
        }
        true
    }

    fn tick_search(&mut self) {
        let request = match &mut self.modal {
            Modal::Search {
                input,
                requested_input,
                dirty_since,
                ..
            } if !input.trim().is_empty()
                && requested_input.as_deref() != Some(input.as_str())
                && dirty_since
                    .is_some_and(|since| since.elapsed() >= Duration::from_millis(250)) =>
            {
                *requested_input = Some(input.clone());
                *dirty_since = None;
                Some(input.clone())
            }
            _ => None,
        };
        if let Some(input) = request {
            self.request_search(input);
        }
    }

    fn apply_responses(&mut self) -> bool {
        let mut changed = false;
        for response in self.worker.drain() {
            match response {
                StockResponse::Refresh { generation, result } => {
                    if let Some(index) = self
                        .columns
                        .iter()
                        .position(|column| column.refresh_generation == generation)
                    {
                        let market = self.columns[index].market;
                        let next_refresh = next_refresh_deadline(
                            self.columns[index].request_started,
                            self.refresh_interval(market),
                            Instant::now(),
                        );
                        let column = &mut self.columns[index];
                        column.fetching = false;
                        match result {
                            Ok(snapshot) => {
                                apply_snapshot(column, snapshot);
                                column.last_update = Some(Instant::now());
                            }
                            Err(error) => column.last_error = Some(error),
                        }
                        column.next_refresh = next_refresh;
                        changed = true;
                    }
                }
                StockResponse::Search { generation, result }
                    if generation == self.search_generation =>
                {
                    match result {
                        Ok(found) => {
                            if let Modal::Search {
                                results, cursor, ..
                            } = &mut self.modal
                            {
                                *results = found;
                                *cursor = 0;
                            }
                        }
                        Err(error) => self.hint = Some(error),
                    }
                    changed = true;
                }
                StockResponse::Detail { generation, result }
                    if generation == self.detail_generation =>
                {
                    self.detail_fetching = false;
                    match result {
                        Ok(detail) => {
                            if let View::Detail { bundle, .. } = &mut self.view {
                                *bundle = Some(detail);
                            }
                        }
                        Err(error) => self.hint = Some(error),
                    }
                    changed = true;
                }
                StockResponse::LiveDetail { generation, result }
                    if generation == self.detail_generation =>
                {
                    self.detail_fetching = false;
                    match result {
                        Ok(live) => {
                            if let View::Detail {
                                bundle: Some(bundle),
                                ..
                            } = &mut self.view
                            {
                                bundle.apply_live(live);
                            }
                        }
                        Err(error) => self.hint = Some(error),
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Return the column index whose cached body rect contains the point, if
    /// any. Used to hit-test the mouse wheel against the three side-by-side
    /// bodies rather than a single monolithic pane rect.
    fn column_at(&self, x: u16, y: u16) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| point_in_rect(x, y, column.body_rect))
    }
}

fn next_refresh_deadline(started: Instant, interval: Duration, now: Instant) -> Instant {
    let deadline = started + interval;
    if deadline > now { deadline } else { now }
}

fn apply_snapshot(column: &mut ColumnState, snapshot: Snapshot) {
    column.rows = snapshot
        .rows
        .into_iter()
        .filter(|row| row.market == column.market)
        .collect();
    column.indices = snapshot.indices;
    column.last_error = None;
    if column.rows.is_empty() {
        column.cursor = 0;
        column.scroll = 0;
    } else {
        let max = column.rows.len() - 1;
        column.cursor = column.cursor.min(max);
        column.scroll = column.scroll.min(max);
    }
}

impl PaneProvider for StockPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        "stock"
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let border = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        match &self.view {
            View::List => render_list(self, area, frame, border),
            View::Detail {
                bundle,
                entry,
                news_cursor,
            } => render_detail(
                area,
                frame,
                border,
                entry,
                bundle.as_ref(),
                *news_cursor,
                self.detail_fetching,
            ),
        }
        self.hint = None;
        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if matches!(self.modal, Modal::Search { .. }) {
            return self.on_search_key(key);
        }
        if let Modal::ConfirmDelete(entry) = &self.modal {
            let entry = entry.clone();
            return match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.watchlist.remove(entry.market, &entry.symbol);
                    self.save_watchlist();
                    self.modal = Modal::None;
                    let column = &mut self.columns[self.focused];
                    column.cursor = column.cursor.saturating_sub(1);
                    true
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.modal = Modal::None;
                    true
                }
                _ => false,
            };
        }
        if matches!(self.view, View::Detail { .. }) {
            return match key.code {
                KeyCode::Left | KeyCode::Esc | KeyCode::Backspace => {
                    self.view = View::List;
                    true
                }
                KeyCode::Char('r') | KeyCode::F(5) => {
                    if let View::Detail { entry, .. } = &self.view {
                        self.request_detail(entry.clone());
                    }
                    true
                }
                KeyCode::Up => {
                    if let View::Detail { news_cursor, .. } = &mut self.view {
                        *news_cursor = news_cursor.saturating_sub(1);
                    }
                    true
                }
                KeyCode::Down => {
                    if let View::Detail {
                        bundle: Some(bundle),
                        news_cursor,
                        ..
                    } = &mut self.view
                    {
                        *news_cursor = (*news_cursor + 1).min(bundle.news.len().saturating_sub(1));
                    }
                    true
                }
                _ => false,
            };
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                true
            }
            KeyCode::PageDown => {
                self.move_cursor(10);
                true
            }
            KeyCode::PageUp => {
                self.move_cursor(-10);
                true
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.columns[self.focused].cursor = 0;
                true
            }
            KeyCode::End | KeyCode::Char('G') => {
                let column = &mut self.columns[self.focused];
                column.cursor = column.rows.len().saturating_sub(1);
                true
            }
            KeyCode::Tab => {
                self.cycle_focus(key.modifiers.contains(KeyModifiers::SHIFT));
                true
            }
            KeyCode::Char('1') => {
                self.focus_market(Market::AShare);
                true
            }
            KeyCode::Char('2') => {
                self.focus_market(Market::HongKong);
                true
            }
            KeyCode::Char('3') => {
                self.focus_market(Market::Us);
                true
            }
            KeyCode::Char('/') => {
                self.modal = Modal::Search {
                    input: String::new(),
                    results: Vec::new(),
                    cursor: 0,
                    requested_input: None,
                    dirty_since: None,
                };
                true
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(entry) = self.selected_entry() {
                    self.modal = Modal::ConfirmDelete(entry);
                }
                true
            }
            KeyCode::Right | KeyCode::Enter => {
                self.open_detail();
                true
            }
            KeyCode::Char('r') | KeyCode::F(5) => {
                self.request_all_refresh(true);
                true
            }
            _ => false,
        }
    }

    fn on_mouse(&mut self, event: MouseEvent, _outer_rect: Rect) -> bool {
        let Some(column_index) = self.column_at(event.column, event.row) else {
            return false;
        };
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.focused = column_index;
                self.columns[column_index].scroll_by(WHEEL_STEP);
                true
            }
            MouseEventKind::ScrollUp => {
                self.focused = column_index;
                self.columns[column_index].scroll_by(-WHEEL_STEP);
                true
            }
            MouseEventKind::Down(_) => {
                self.focused = column_index;
                true
            }
            _ => false,
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            let now = Instant::now();
            for column in &mut self.columns {
                column.next_refresh = now;
            }
        }
    }

    fn reload(&mut self) {
        self.request_all_refresh(true);
    }

    fn poll_background(&mut self) -> bool {
        self.tick_search();
        let changed = self.apply_responses();
        let now = Instant::now();
        match &self.view {
            View::List => {
                let due: Vec<usize> = (0..self.columns.len())
                    .filter(|&index| {
                        let column = &self.columns[index];
                        !column.fetching && now >= column.next_refresh
                    })
                    .collect();
                for column_index in due {
                    self.request_column_refresh(column_index);
                }
            }
            View::Detail { entry, .. } if !self.detail_fetching => {
                let entry = entry.clone();
                self.request_live_detail(entry);
            }
            _ => {}
        }
        changed
    }
}

fn render_list(pane: &mut StockPane, area: Rect, frame: &mut Frame<'_>, border: Style) {
    let focused_market = pane.focused_market();
    let block = Block::default()
        .title(format!(
            " stock · A股 · 港股 · 美股 · focus:{} ",
            focused_market.label()
        ))
        .borders(Borders::ALL)
        .border_style(border);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    render_indices(frame, split[0], pane);
    render_columns(frame, split[1], pane);
    render_list_footer(frame, split[2], pane);
}

fn render_indices(frame: &mut Frame<'_>, area: Rect, pane: &StockPane) {
    let column = &pane.columns[pane.focused];
    let mut spans = vec![Span::styled(
        format!("大盘 [{}] ", column.market.label()),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )];
    for index in column.indices.iter().take(4) {
        spans.push(Span::styled(
            format!(
                "{} {} {}  ",
                index.name,
                format_price(Some(index.close)),
                format_change_pct(Some(index.change_pct))
            ),
            change_style(index.change_pct),
        ));
    }
    if column.fetching && column.indices.is_empty() {
        spans.push(Span::styled(
            "loading...",
            Style::default().fg(Color::DarkGray),
        ));
    }
    Paragraph::new(Line::from(spans))
        .wrap(Wrap { trim: true })
        .render(area, frame.buffer_mut());
}

fn render_columns(frame: &mut Frame<'_>, area: Rect, pane: &mut StockPane) {
    // Reset every column's body rect so a column that is off-screen (narrow
    // terminal, focused-only fallback) is not mouse-hit-testable.
    for column in &mut pane.columns {
        column.body_rect = Rect::default();
    }
    if area.width >= MIN_MULTI_COLUMN_WIDTH {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(area);
        for (offset, column_index) in (0..pane.columns.len()).enumerate() {
            let rect = split[offset];
            let focused = column_index == pane.focused;
            let (state_line, column_open) = column_status(pane, column_index);
            render_column(
                frame,
                rect,
                &mut pane.columns[column_index],
                focused,
                state_line,
                column_open,
            );
        }
    } else {
        // Narrow terminals: show only the focused column but keep every
        // column's cached state so widening restores side-by-side.
        let focused = pane.focused;
        let (state_line, column_open) = column_status(pane, focused);
        render_column(
            frame,
            area,
            &mut pane.columns[focused],
            true,
            state_line,
            column_open,
        );
    }
}

/// Compute the per-column title suffix from market state and the most recent
/// completed refresh. This reports observed freshness instead of promising the
/// configured request rate.
fn column_status(pane: &StockPane, column_index: usize) -> (String, bool) {
    let column = &pane.columns[column_index];
    let open = column.market.is_open_at(Utc::now());
    let market_state = if open { "open" } else { "closed" };
    let freshness = column.last_update.map_or_else(
        || "awaiting update".to_string(),
        |updated| format!("updated {} ago", format_update_age(updated.elapsed())),
    );
    (format!("{market_state} · {freshness}"), open)
}

fn format_update_age(age: Duration) -> String {
    if age < Duration::from_secs(10) {
        format!("{:.1}s", age.as_secs_f64())
    } else {
        format!("{}s", age.as_secs())
    }
}

fn render_column(
    frame: &mut Frame<'_>,
    area: Rect,
    column: &mut ColumnState,
    focused: bool,
    state_line: String,
    _column_open: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = format!(" {} · {} ", column.market.label(), state_line);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width == 0 || inner.height == 0 {
        column.body_rect = Rect::default();
        return;
    }
    column.body_rect = inner;
    let viewport = inner.height.saturating_sub(1) as usize;
    column.ensure_cursor_visible(viewport);
    if column.rows.is_empty() {
        let text = if column.fetching {
            "loading..."
        } else if column.last_error.is_some() {
            column.last_error.as_deref().unwrap_or("")
        } else {
            "no watchlist entries"
        };
        Paragraph::new(text)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true })
            .render(inner, frame.buffer_mut());
        return;
    }
    let scroll = column.scroll;
    let end = (scroll + viewport.max(1)).min(column.rows.len());
    let cursor = column.cursor;
    let rendered: Vec<Row> = column.rows[scroll..end]
        .iter()
        .enumerate()
        .map(|(offset, quote)| {
            let selected = scroll + offset == cursor;
            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                quote.symbol.clone(),
                quote.name.clone(),
                format_price(quote.last),
                format_change_pct(quote.change_pct),
                format_compact(quote.volume),
            ])
            .style(base.patch(quote.change_pct.map_or_else(Style::default, change_style)))
        })
        .collect();
    Table::new(
        rendered,
        [
            Constraint::Length(9),
            Constraint::Min(6),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(6),
        ],
    )
    .header(
        Row::new(["CODE", "NAME", "LAST", "CHG%", "VOL"])
            .style(Style::default().fg(Color::DarkGray)),
    )
    .column_spacing(1)
    .render(inner, frame.buffer_mut());
    if column.rows.len() > viewport && inner.width > 1 && viewport > 0 {
        let mut state = ScrollbarState::new(column.rows.len())
            .position(column.scroll)
            .viewport_content_length(viewport);
        ratatui::widgets::StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            inner,
            frame.buffer_mut(),
            &mut state,
        );
    }
}

fn render_list_footer(frame: &mut Frame<'_>, area: Rect, pane: &StockPane) {
    let (text, style) = match &pane.modal {
        Modal::Search {
            input,
            results,
            cursor,
            ..
        } => {
            let selected = results.get(*cursor).map_or("", |row| row.name.as_str());
            (
                format!(
                    "/ {input}_  {selected}  ({})",
                    pane.focused_market().label()
                ),
                Style::default().fg(Color::Cyan),
            )
        }
        Modal::ConfirmDelete(entry) => (
            format!(
                "delete {} {}? Enter/y confirm · Esc/n cancel",
                entry.symbol, entry.name
            ),
            Style::default().fg(Color::Yellow),
        ),
        Modal::None => (
            pane.hint.clone().unwrap_or_else(|| {
                "Tab 切栏 · / search · d delete · ↑↓ move · 滚轮滚动 · →/Enter details · r refresh"
                    .into()
            }),
            Style::default().fg(Color::DarkGray),
        ),
    };
    Paragraph::new(Line::styled(text, style)).render(area, frame.buffer_mut());
}

fn render_detail(
    area: Rect,
    frame: &mut Frame<'_>,
    border: Style,
    entry: &WatchEntry,
    detail: Option<&DetailBundle>,
    news_cursor: usize,
    fetching: bool,
) {
    let block = Block::default()
        .title(format!(
            " stock · {} {} · ←/Esc back ",
            entry.symbol, entry.name
        ))
        .borders(Borders::ALL)
        .border_style(border);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let Some(detail) = detail else {
        Paragraph::new(if fetching {
            "loading details..."
        } else {
            "details unavailable"
        })
        .style(Style::default().fg(Color::DarkGray))
        .render(inner, frame.buffer_mut());
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(inner.height.saturating_div(3).max(2)),
        ])
        .split(inner);
    render_quote_header(frame, rows[0], &detail.quote, detail.fundamentals.as_ref());
    let charts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    render_wave(frame, charts[0], detail);
    render_kline(frame, charts[1], detail);
    render_news(frame, rows[2], &detail.news, news_cursor);
}

fn render_quote_header(
    frame: &mut Frame<'_>,
    area: Rect,
    quote: &QuoteRow,
    fundamentals: Option<&rimeterm_stock::Fundamentals>,
) {
    let price = format_price(quote.last);
    let change = format_change_pct(quote.change_pct);
    let metrics = fundamentals.map_or_else(String::new, |value| {
        format!(
            "  PE {}  PB {}  MCap {} {}",
            format_price(value.pe),
            format_price(value.pb),
            format_compact(value.market_cap),
            value.currency,
        )
    });
    Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                format!("Last {price}  {change}"),
                quote.change_pct.map_or_else(Style::default, change_style),
            ),
            Span::raw(format!(
                "  O {} H {} L {}",
                optional_price(quote.open),
                optional_price(quote.high),
                optional_price(quote.low)
            )),
        ]),
        Line::from(format!(
            "Vol {}  Amount {}{metrics}",
            optional_compact(quote.volume),
            optional_compact(quote.amount)
        )),
    ])
    .render(area, frame.buffer_mut());
}

fn render_wave(frame: &mut Frame<'_>, area: Rect, detail: &DetailBundle) {
    let data: Vec<(f64, f64)> = detail
        .intraday
        .iter()
        .enumerate()
        .map(|(index, point)| (index as f64, point.price))
        .collect();
    let bounds = y_bounds(data.iter().map(|(_, y)| *y));
    let datasets = vec![
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&data),
    ];
    Chart::new(datasets)
        .block(
            Block::default()
                .title(" 波形图 · 5m ")
                .borders(Borders::ALL),
        )
        .legend_position(None)
        .x_axis(Axis::default().bounds([0.0, data.len().saturating_sub(1).max(1) as f64]))
        .y_axis(Axis::default().bounds(bounds))
        .render(area, frame.buffer_mut());
}

fn render_kline(frame: &mut Frame<'_>, area: Rect, detail: &DetailBundle) {
    let candles = &detail.candles;
    let block = Block::default()
        .title(" K线图 · daily ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if candles.is_empty() || inner.width == 0 || inner.height == 0 {
        return;
    }
    let count = candles.len().min(inner.width as usize);
    let slice = &candles[candles.len() - count..];
    let bounds = y_bounds(slice.iter().flat_map(|candle| [candle.low, candle.high]));
    let scale = (bounds[1] - bounds[0]).max(f64::EPSILON);
    for (offset, candle) in slice.iter().enumerate() {
        let x = inner.x + offset as u16;
        let to_y = |price: f64| {
            inner.y + inner.height.saturating_sub(1)
                - (((price - bounds[0]) / scale * f64::from(inner.height.saturating_sub(1))) as u16)
                    .min(inner.height.saturating_sub(1))
        };
        let high = to_y(candle.high);
        let low = to_y(candle.low);
        let open = to_y(candle.open);
        let close = to_y(candle.close);
        let style = change_style(candle.close - candle.open);
        for y in high..=low {
            frame.buffer_mut()[(x, y)].set_symbol("│").set_style(style);
        }
        let (top, bottom) = if open <= close {
            (open, close)
        } else {
            (close, open)
        };
        for y in top..=bottom {
            frame.buffer_mut()[(x, y)].set_symbol("█").set_style(style);
        }
    }
}

fn render_news(frame: &mut Frame<'_>, area: Rect, news: &[NewsItem], cursor: usize) {
    let block = Block::default().title(" 资讯 ").borders(Borders::TOP);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let lines: Vec<Line> = news
        .iter()
        .skip(cursor)
        .take(inner.height as usize)
        .map(|item| {
            Line::from(format!(
                "{}  {}  {}",
                item.published_at, item.title, item.source
            ))
        })
        .collect();
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .render(inner, frame.buffer_mut());
}

fn optional_price(value: Option<f64>) -> String {
    format_price(value)
}

fn optional_compact(value: Option<f64>) -> String {
    format_compact(value)
}

fn change_style(change: f64) -> Style {
    if change > 0.0 {
        Style::default().fg(Color::Green)
    } else if change < 0.0 {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn y_bounds(values: impl Iterator<Item = f64>) -> [f64; 2] {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for value in values.filter(|value| value.is_finite()) {
        min = min.min(value);
        max = max.max(value);
    }
    if !min.is_finite() || !max.is_finite() {
        return [0.0, 1.0];
    }
    if (max - min).abs() < f64::EPSILON {
        return [min - 1.0, max + 1.0];
    }
    let padding = (max - min) * 0.05;
    [min - padding, max + padding]
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.right()
        && y >= rect.y
        && y < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    fn pane() -> StockPane {
        let directory = tempfile::tempdir().unwrap().keep();
        StockPane::new(StockConfig::default(), directory.join("watchlist.toml"))
    }

    fn quote(market: Market, symbol: &str, name: &str) -> QuoteRow {
        QuoteRow {
            market,
            symbol: symbol.into(),
            name: name.into(),
            last: Some(1.0),
            change_pct: Some(1.0),
            change_amount: Some(0.1),
            open: Some(1.0),
            high: Some(2.0),
            low: Some(0.5),
            prev_close: Some(0.9),
            volume: Some(1.0),
            amount: Some(1.0),
            pe: None,
            pb: None,
            market_cap: None,
            as_of: None,
            error: None,
        }
    }

    fn seed_columns(pane: &mut StockPane) {
        pane.columns[0].rows = (0..5)
            .map(|i| quote(Market::AShare, &format!("60000{i}"), &format!("A{i}")))
            .collect();
        pane.columns[1].rows = (0..5)
            .map(|i| quote(Market::HongKong, &format!("0070{i}"), &format!("HK{i}")))
            .collect();
        pane.columns[2].rows = (0..5)
            .map(|i| quote(Market::Us, &format!("AAP{i}"), &format!("US{i}")))
            .collect();
    }

    #[test]
    fn tab_cycles_focus_between_three_markets_without_touching_cursors() {
        let mut pane = pane();
        seed_columns(&mut pane);
        pane.columns[0].cursor = 2;
        pane.columns[1].cursor = 3;
        pane.columns[2].cursor = 4;
        pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(pane.focused_market(), Market::HongKong);
        pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(pane.focused_market(), Market::Us);
        pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(pane.focused_market(), Market::AShare);
        pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(pane.focused_market(), Market::Us);
        assert_eq!(pane.columns[0].cursor, 2);
        assert_eq!(pane.columns[1].cursor, 3);
        assert_eq!(pane.columns[2].cursor, 4);
    }

    #[test]
    fn arrow_keys_only_move_focused_column_cursor() {
        let mut pane = pane();
        seed_columns(&mut pane);
        pane.focused = 1;
        pane.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            (
                pane.columns[0].cursor,
                pane.columns[1].cursor,
                pane.columns[2].cursor
            ),
            (0, 2, 0)
        );
    }

    #[test]
    fn wheel_scrolls_column_under_cursor_and_focuses_it() {
        let mut pane = pane();
        seed_columns(&mut pane);
        // Simulate a rendered layout: three horizontally adjacent bodies.
        pane.columns[0].body_rect = Rect::new(0, 2, 30, 3);
        pane.columns[1].body_rect = Rect::new(30, 2, 30, 3);
        pane.columns[2].body_rect = Rect::new(60, 2, 30, 3);
        pane.on_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 61,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 90, 20),
        );
        assert_eq!(pane.focused_market(), Market::Us);
        assert!(pane.columns[2].scroll >= 1);
        assert_eq!(pane.columns[0].scroll, 0);
        assert_eq!(pane.columns[1].scroll, 0);
    }

    #[test]
    fn wheel_outside_any_column_is_ignored() {
        let mut pane = pane();
        seed_columns(&mut pane);
        pane.columns[0].body_rect = Rect::new(0, 2, 30, 3);
        pane.columns[1].body_rect = Rect::new(30, 2, 30, 3);
        pane.columns[2].body_rect = Rect::new(60, 2, 30, 3);
        let handled = pane.on_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 200,
                row: 200,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 90, 20),
        );
        assert!(!handled);
        assert!(pane.columns.iter().all(|column| column.scroll == 0));
    }

    #[test]
    fn left_click_transfers_focus_without_scrolling() {
        let mut pane = pane();
        seed_columns(&mut pane);
        pane.columns[0].body_rect = Rect::new(0, 2, 30, 3);
        pane.columns[1].body_rect = Rect::new(30, 2, 30, 3);
        pane.columns[2].body_rect = Rect::new(60, 2, 30, 3);
        pane.on_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 31,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 90, 20),
        );
        assert_eq!(pane.focused_market(), Market::HongKong);
        assert!(pane.columns.iter().all(|column| column.scroll == 0));
    }

    #[test]
    fn three_columns_render_all_market_labels_side_by_side() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut pane = pane();
        for column in &mut pane.columns {
            column.fetching = false;
        }
        seed_columns(&mut pane);
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal
            .draw(|frame| {
                let ctx = PaneRenderCtx {
                    focused: true,
                    title_override: None,
                    focus_color: Color::Cyan,
                };
                pane.render(frame.area(), frame, &ctx);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(
            text.contains("A")
                && text.contains("港")
                && text.contains("美")
                && text.contains("A0")
                && text.contains("HK0")
                && text.contains("US0"),
            "{text}"
        );
    }

    #[test]
    fn narrow_terminal_falls_back_to_focused_column_but_keeps_state() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut pane = pane();
        for column in &mut pane.columns {
            column.fetching = false;
        }
        seed_columns(&mut pane);
        pane.focused = 2;
        pane.columns[0].cursor = 3;
        pane.columns[1].cursor = 4;
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal
            .draw(|frame| {
                let ctx = PaneRenderCtx {
                    focused: true,
                    title_override: None,
                    focus_color: Color::Cyan,
                };
                pane.render(frame.area(), frame, &ctx);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("US0"), "focused column visible: {text}");
        assert!(!text.contains("A0"), "non-focused column hidden: {text}");
        assert_eq!(pane.columns[0].cursor, 3);
        assert_eq!(pane.columns[1].cursor, 4);
    }

    #[test]
    fn enter_opens_detail_for_focused_column_selection() {
        let mut pane = pane();
        seed_columns(&mut pane);
        pane.focused = 1;
        pane.columns[1].cursor = 2;
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match &pane.view {
            View::Detail { entry, .. } => {
                assert_eq!(entry.market, Market::HongKong);
                assert_eq!(entry.symbol, "00702");
            }
            View::List => panic!("expected Detail view"),
        }
    }

    #[test]
    fn hidden_tab_uses_closed_rate_or_slower() {
        let mut pane = pane();
        pane.visible = false;
        assert!(pane.refresh_interval(Market::AShare) >= Duration::from_secs(60));
    }

    #[test]
    fn refresh_deadline_is_anchored_to_request_start() {
        let request_started = Instant::now() - Duration::from_millis(750);
        let interval = Duration::from_secs(1);

        let deadline = next_refresh_deadline(request_started, interval, Instant::now());

        assert!(deadline.duration_since(Instant::now()) <= Duration::from_millis(260));
    }

    #[test]
    fn column_status_reports_observed_update_age_not_promised_rate() {
        let mut pane = pane();
        pane.visible = true;
        pane.columns[2].last_update = Some(Instant::now() - Duration::from_millis(1_500));

        let (status, _) = column_status(&pane, 2);

        assert!(
            status.contains("updated 1.5s ago") && !status.contains("1Hz"),
            "{status}"
        );
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .filter_map(|cell| {
                let symbol = cell.symbol();
                (!symbol.is_empty()).then_some(symbol)
            })
            .collect()
    }
}
