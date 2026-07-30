//! Native AI-model catalog pane — a compact flat table showing every
//! model exposed via [`https://models.dev/api.json`](https://models.dev/api.json)
//! with provider, context window, and $/1M-tokens pricing.
//!
//! Modelled on [`crate::agtop_pane::AgtopPane`]: the pane owns a
//! [`ModelsWorker`] on a background OS thread that hits the models.dev
//! API; the pane pulls the resulting [`Snapshot`] and renders a filtered
//! / sorted view. Keybindings mirror the sysmon / agtop process tables
//! so muscle memory carries over:
//!
//! | Key             | Action                                            |
//! |-----------------|---------------------------------------------------|
//! | `j / k / ↓ ↑`   | move cursor                                       |
//! | `p n c i o d`   | sort by provider / name / context / input / output / date |
//! | `Tab`           | flip sort direction                               |
//! | `/`             | enter filter mode (matches id/name/provider)      |
//! | `Enter`         | commit filter                                     |
//! | `Esc`           | dismiss filter                                    |
//! | `r` / `F5`      | force a refetch from models.dev                   |
//!
//! Attribution: the fetched schema + row shape are a direct port of
//! MIT-licensed `reyamira/models` (`modelsdev` v0.14.0). See
//! [`rimeterm_models`] for details.
//!
//! [`ModelsWorker`]: crate::models_worker::ModelsWorker
//! [`Snapshot`]: crate::models_model::Snapshot

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        StatefulWidget, Table, Widget,
    },
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use rimeterm_models::format::{format_context, format_cost_short};

use crate::models_model::{
    ModelRow, ModelView, ModelsRequest, ModelsResponse, Snapshot, SortKey, SortOrder,
};

use crate::models_worker::ModelsWorker;

/// Rows the scroll wheel advances per notch. Matches `pty_pane`'s
/// three-line convention so the "one notch = a chunk" muscle memory
/// stays consistent across every rimeterm pane that scrolls.
const WHEEL_STEP: i32 = 3;

/// Modal state — only filter entry uses one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Modal {
    #[default]
    None,
    Filter {
        input: String,
    },
}

/// Native `ModelsPane` provider — the models.dev browser for rimeterm.
pub struct ModelsPane {
    id: PaneId,
    title: String,
    worker: ModelsWorker,
    snapshot: Snapshot,
    /// Monotonic counter; bumped before every `Fetch` request so late
    /// replies land harmlessly when they overlap a fresher one.
    requested_generation: u64,
    applied_generation: u64,
    sort_key: SortKey,
    sort_order: SortOrder,
    filter: Option<String>,
    cursor: usize,
    modal: Modal,
    /// True while a fetch is in flight — drives the "loading" hint.
    fetching: bool,
    /// Transient status text rendered on the footer row. Cleared after
    /// the next render pass consumes it.
    hint: Option<String>,
    /// Rect of the table body captured on the last render — used by
    /// [`Self::on_mouse`] to hit-test scroll-wheel events without
    /// depending on the App's outer rect (which includes the border
    /// row that the child shouldn't scroll through).
    body_rect: Rect,
}

impl ModelsPane {
    pub fn new() -> Self {
        let worker = ModelsWorker::spawn();
        // Prime the counter with a first fetch immediately so the
        // pane doesn't render an empty "no data" state on startup.
        let requested_generation = 1;
        worker.send(ModelsRequest::Fetch {
            generation: requested_generation,
        });
        Self {
            id: PaneId::next(),
            title: "models".to_owned(),
            worker,
            snapshot: Snapshot::empty(),
            requested_generation,
            applied_generation: 0,
            sort_key: SortKey::Provider,
            sort_order: SortOrder::Ascending,
            filter: None,
            cursor: 0,
            modal: Modal::None,
            fetching: true,
            hint: None,
            body_rect: Rect::default(),
        }
    }

    fn model_view(&self) -> ModelView {
        ModelView::from_snapshot(
            &self.snapshot,
            self.sort_key,
            self.sort_order,
            self.filter.as_deref(),
        )
    }

    fn request_fetch(&mut self) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(ModelsRequest::Fetch {
            generation: self.requested_generation,
        });
        self.fetching = true;
    }

    fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_order = self.sort_order.flip();
        } else {
            self.sort_key = key;
            // Provider / Name default to ascending (A→Z); numeric
            // columns default to descending (big first) — matches
            // what a user reaching for `c` (context) or `i` (cost)
            // typically wants: "show me the biggest / cheapest first".
            self.sort_order = match key {
                SortKey::Provider | SortKey::Name => SortOrder::Ascending,
                _ => SortOrder::Descending,
            };
        }
        self.cursor = 0;
    }

    fn set_hint<S: Into<String>>(&mut self, text: S) {
        self.hint = Some(text.into());
    }

    /// Move the cursor by a signed row delta, clamping into
    /// `[0, view.rows.len())`. Shared by keyboard PageUp/PageDown
    /// and the mouse-wheel handler so both paths behave identically.
    fn move_cursor(&mut self, delta: i32) {
        let view = self.model_view();
        if view.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = view.rows.len() - 1;
        let next = (self.cursor as i32).saturating_add(delta).max(0) as usize;
        self.cursor = next.min(max);
    }
}

impl Default for ModelsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneProvider for ModelsPane {
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
        let border_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let title = format_title(
            &self.snapshot,
            self.sort_key,
            self.sort_order,
            self.fetching,
        );
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.height == 0 || inner.width == 0 {
            return RenderOutcome::default();
        }

        // Bottom row: filter footer / hint / selected-model detail
        // whenever there's something to show. Rest of the inner rect
        // goes to the table.
        let footer_active = self.hint.is_some()
            || !matches!(self.modal, Modal::None)
            || self.snapshot.last_error.is_some()
            || !self.snapshot.rows.is_empty();
        let (body_rect, footer_rect) = if footer_active && inner.height >= 2 {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(inner);
            (split[0], Some(split[1]))
        } else {
            (inner, None)
        };

        let view = self.model_view();
        // Cache the body rect so on_mouse can hit-test wheel events
        // against the scrollable region only (not the border, not the
        // footer). Empty-state paints in the same rect and doesn't
        // scroll, but caching it uniformly keeps the pane's mouse
        // ownership shape simple.
        self.body_rect = body_rect;
        render_models_table(
            frame,
            body_rect,
            &view,
            self.cursor,
            self.fetching,
            self.snapshot.last_error.as_deref(),
        );

        if let Some(rect) = footer_rect {
            let selected = view.rows.get(self.cursor);
            render_footer(
                frame,
                rect,
                &self.modal,
                self.hint.as_deref(),
                self.snapshot.last_error.as_deref(),
                selected,
            );
        }
        // One-shot hints clear after the frame that displayed them.
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

        match &mut self.modal {
            Modal::Filter { input } => match key.code {
                KeyCode::Char(c) => {
                    input.push(c);
                    true
                }
                KeyCode::Backspace => {
                    input.pop();
                    true
                }
                KeyCode::Enter => {
                    self.filter = if input.is_empty() {
                        None
                    } else {
                        Some(input.clone())
                    };
                    self.cursor = 0;
                    self.modal = Modal::None;
                    true
                }
                KeyCode::Esc => {
                    self.modal = Modal::None;
                    true
                }
                _ => false,
            },
            Modal::None => on_key_default(self, key),
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent, _outer_rect: Rect) -> bool {
        // Route wheel events through cursor-motion (which the
        // window_around() logic reflects in the visible viewport)
        // rather than a separate scroll offset — one source of
        // truth means the highlight and the scrollbar thumb can
        // never drift out of sync. Same shape as `fr_pane` and
        // `agtop_pane`'s wheel handling.
        if !point_in_rect(ev.column, ev.row, self.body_rect) {
            return false;
        }
        match ev.kind {
            MouseEventKind::ScrollDown => {
                self.move_cursor(WHEEL_STEP);
                true
            }
            MouseEventKind::ScrollUp => {
                self.move_cursor(-WHEEL_STEP);
                true
            }
            _ => false,
        }
    }

    fn reload(&mut self) {
        self.request_fetch();
        self.set_hint("↻ refreshing");
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;

        for response in self.worker.drain() {
            let ModelsResponse::Fetch { generation, result } = response;
            if generation < self.applied_generation {
                // Stale — a fresher snapshot has already been applied.
                continue;
            }
            self.applied_generation = generation;
            self.fetching = false;
            match result {
                Ok(mut snapshot) => {
                    // Preserve last_error only if the new snapshot is
                    // empty (transient blip) — a good refetch clears it.
                    snapshot.last_error = None;
                    self.snapshot = snapshot;
                }
                Err(msg) => {
                    // Keep any previously-fetched rows on screen;
                    // just tag the snapshot with the error so the
                    // footer shows what went wrong.
                    self.snapshot.last_error = Some(msg);
                }
            }
            // Clamp cursor in case the row count shrank.
            let view = self.model_view();
            if self.cursor >= view.rows.len() {
                self.cursor = view.rows.len().saturating_sub(1);
            }
            changed = true;
        }
        changed
    }
}

fn on_key_default(pane: &mut ModelsPane, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let view = pane.model_view();
            if !view.rows.is_empty() {
                pane.cursor = (pane.cursor + 1).min(view.rows.len() - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            pane.cursor = pane.cursor.saturating_sub(1);
            true
        }
        KeyCode::PageDown => {
            let view = pane.model_view();
            if !view.rows.is_empty() {
                pane.cursor = (pane.cursor + 10).min(view.rows.len() - 1);
            }
            true
        }
        KeyCode::PageUp => {
            pane.cursor = pane.cursor.saturating_sub(10);
            true
        }
        KeyCode::Home | KeyCode::Char('g') => {
            pane.cursor = 0;
            true
        }
        KeyCode::End | KeyCode::Char('G') => {
            let view = pane.model_view();
            if !view.rows.is_empty() {
                pane.cursor = view.rows.len() - 1;
            }
            true
        }
        KeyCode::Tab => {
            pane.sort_order = pane.sort_order.flip();
            true
        }
        KeyCode::Char('p') => {
            pane.set_sort(SortKey::Provider);
            true
        }
        KeyCode::Char('n') => {
            pane.set_sort(SortKey::Name);
            true
        }
        KeyCode::Char('c') => {
            pane.set_sort(SortKey::Context);
            true
        }
        KeyCode::Char('i') => {
            pane.set_sort(SortKey::InputCost);
            true
        }
        KeyCode::Char('o') => {
            pane.set_sort(SortKey::OutputCost);
            true
        }
        KeyCode::Char('d') => {
            pane.set_sort(SortKey::Release);
            true
        }
        KeyCode::Char('/') => {
            pane.modal = Modal::Filter {
                input: pane.filter.clone().unwrap_or_default(),
            };
            true
        }
        KeyCode::Char('r') | KeyCode::F(5) => {
            pane.request_fetch();
            pane.set_hint("↻ refetching from models.dev");
            true
        }
        _ => false,
    }
}

/// Compose the tab-title-plus-summary shown in the border. Kept short
/// so it doesn't push the sort indicator past the border on a narrow pane.
fn format_title(snapshot: &Snapshot, key: SortKey, order: SortOrder, fetching: bool) -> String {
    let arrow = match order {
        SortOrder::Ascending => "↑",
        SortOrder::Descending => "↓",
    };
    let key_name = match key {
        SortKey::Provider => "prov",
        SortKey::Name => "name",
        SortKey::Context => "ctx",
        SortKey::InputCost => "in$",
        SortKey::OutputCost => "out$",
        SortKey::Release => "date",
    };
    let loading = if fetching { " · loading" } else { "" };
    format!(
        " models · {} providers · {} models · sort:{}{}{} ",
        snapshot.provider_count,
        snapshot.rows.len(),
        key_name,
        arrow,
        loading,
    )
}

/// Draw the models table with a highlighted cursor row, a right-edge
/// scrollbar when there's more content than the viewport can show,
/// and an empty-state hint when nothing matches.
fn render_models_table(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &ModelView,
    cursor: usize,
    fetching: bool,
    last_error: Option<&str>,
) {
    if view.rows.is_empty() {
        // Precedence: user filter (typed intent) > fetch error (real
        // failure) > fetching (progress) > default. Showing the
        // filter miss over an error is correct — the user just
        // typed the filter, they know what they asked for.
        let (msg, msg_style, footer) = match view.filter.as_deref() {
            Some(f) if !f.is_empty() => (
                format!("no models match `{f}`"),
                Style::default().fg(Color::DarkGray),
                "sort: p·n·c·i·o·d   filter: /   refetch: r",
            ),
            _ if last_error.is_some() => (
                format!("⚠ {}", last_error.unwrap()),
                Style::default().fg(Color::Red),
                "set HTTPS_PROXY or RIMETERM_MODELS_URL, then press r",
            ),
            _ if fetching => (
                "fetching https://models.dev/api.json…".to_owned(),
                Style::default().fg(Color::DarkGray),
                "sort: p·n·c·i·o·d   filter: /   refetch: r",
            ),
            _ => (
                "no models loaded".to_owned(),
                Style::default().fg(Color::DarkGray),
                "sort: p·n·c·i·o·d   filter: /   refetch: r",
            ),
        };
        let hint = Paragraph::new(vec![
            Line::from(Span::styled(msg, msg_style)),
            Line::from(""),
            Line::from(Span::styled(
                footer,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ])
        .wrap(ratatui::widgets::Wrap { trim: true });
        hint.render(area, frame.buffer_mut());
        return;
    }

    // Reserve the rightmost column for the scrollbar when it would
    // actually do something (row count exceeds the body rows). The
    // scrollbar renders over the same column ratatui would use for
    // the last content cell, so we shave 1 col off `table_area` up
    // front rather than letting the scrollbar clobber cell text.
    let viewport_rows = area.height.saturating_sub(1) as usize; // -1 for header
    let needs_scrollbar = view.rows.len() > viewport_rows && area.width >= 2;
    let (table_area, scrollbar_area) = if needs_scrollbar {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    };

    let header = Row::new(vec![
        cell_dim("PROVIDER"),
        cell_dim("MODEL"),
        cell_dim("CTX"),
        cell_dim(" IN$"),
        cell_dim("OUT$"),
        cell_dim("R"),
    ]);

    // Compute a viewport window so the cursor stays visible even
    // with 4k rows — ratatui's Table has no built-in scroll.
    let (start, end) = window_around(cursor, view.rows.len(), viewport_rows);

    let (provider_w, model_w) = column_widths(table_area.width);

    let rows: Vec<Row> = view.rows[start..end]
        .iter()
        .enumerate()
        .map(|(local_idx, row)| {
            let abs_idx = start + local_idx;
            let selected = abs_idx == cursor;
            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                cell(truncate(&row.provider_name, provider_w), base),
                cell(truncate(&row.model_name, model_w), base),
                cell(format_context(row.context), base),
                cell(
                    format!("{:>5}", format_cost_short(row.input_cost)),
                    cost_style(row.input_cost).patch(base),
                ),
                cell(
                    format!("{:>5}", format_cost_short(row.output_cost)),
                    cost_style(row.output_cost).patch(base),
                ),
                cell(
                    reasoning_glyph(row.reasoning),
                    reasoning_style(row.reasoning).patch(base),
                ),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(provider_w.max(1) as u16),
            Constraint::Length(model_w.max(1) as u16),
            Constraint::Length(5), // ctx, e.g. "128K"
            Constraint::Length(5), // in$
            Constraint::Length(5), // out$
            Constraint::Length(1), // reasoning glyph
        ],
    )
    .header(header)
    .column_spacing(1);
    ratatui::widgets::Widget::render(table, table_area, frame.buffer_mut());

    // Scrollbar. `viewport_content_length` scales the thumb size so
    // it accurately reflects the fraction of rows currently visible;
    // `position` uses the cursor row (not `start`) so grabbing the
    // thumb visually matches where the highlight lives.
    if let Some(sb_area) = scrollbar_area {
        let mut sb_state = ScrollbarState::new(view.rows.len())
            .position(cursor)
            .viewport_content_length(viewport_rows);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .render(sb_area, frame.buffer_mut(), &mut sb_state);
    }
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    modal: &Modal,
    hint: Option<&str>,
    error: Option<&str>,
    selected: Option<&ModelRow>,
) {
    let (text, style) = match modal {
        Modal::Filter { input } => (format!("/ {input}_"), Style::default().fg(Color::Cyan)),
        Modal::None => {
            // Priority: user-set one-shot hint > fetch error > selected-row detail.
            if let Some(h) = hint {
                (h.to_string(), Style::default().fg(Color::Yellow))
            } else if let Some(e) = error {
                (format!("⚠ {e}"), Style::default().fg(Color::Red))
            } else if let Some(row) = selected {
                (
                    format_detail_line(row),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                (String::new(), Style::default())
            }
        }
    };
    Paragraph::new(Line::styled(text, style)).render(area, frame.buffer_mut());
}

/// Compact single-line detail for the currently-selected row.
/// `openai/gpt-4o · 128K ctx · $2.5/$10.0 · text · 2024-05-13`
fn format_detail_line(row: &ModelRow) -> String {
    let ctx = format_context(row.context);
    let cost = format!(
        "{}/{}",
        format_cost_short(row.input_cost),
        format_cost_short(row.output_cost),
    );
    let mut parts = vec![
        format!("{}/{}", row.provider_id, row.model_id),
        format!("{ctx} ctx"),
        cost,
    ];
    let mut caps = Vec::new();
    if row.reasoning {
        caps.push("reasoning");
    }
    if row.tool_call {
        caps.push("tools");
    }
    if row.attachment {
        caps.push("files");
    }
    if row.open_weights {
        caps.push("open");
    }
    if !caps.is_empty() {
        parts.push(caps.join("·"));
    }
    if let Some(date) = row.release_date.as_deref() {
        parts.push(date.to_owned());
    }
    parts.join(" · ")
}

/// Center a `viewport`-sized window around `cursor` in `total`. Falls
/// back to `[0, total)` when the viewport is at least as tall as the
/// data.
fn window_around(cursor: usize, total: usize, viewport: usize) -> (usize, usize) {
    if viewport == 0 || total == 0 {
        return (0, 0);
    }
    if total <= viewport {
        return (0, total);
    }
    let half = viewport / 2;
    let start = cursor.saturating_sub(half);
    let end = (start + viewport).min(total);
    // If we bumped against the end, shift start left so the viewport
    // stays full.
    let start = end.saturating_sub(viewport);
    (start, end)
}

/// Split the row's leftover width between PROVIDER and MODEL columns.
/// Fixed columns consume: 5 (ctx) + 5 (in$) + 5 (out$) + 1 (R) + 5
/// separators = 21 cells. Remainder gets split 40/60 provider/model
/// with a minimum of 6 for each so a tiny pane still renders both.
fn column_widths(inner_width: u16) -> (usize, usize) {
    const FIXED: u16 = 21;
    let remaining = inner_width.saturating_sub(FIXED) as usize;
    let provider = (remaining * 40 / 100).max(6);
    let model = remaining.saturating_sub(provider).max(6);
    (provider, model)
}

fn cell<S: Into<String>>(text: S, style: Style) -> ratatui::widgets::Cell<'static> {
    ratatui::widgets::Cell::from(Line::styled(text.into(), style))
}

fn cell_dim(text: &'static str) -> ratatui::widgets::Cell<'static> {
    cell(
        text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn cost_style(cost: Option<f64>) -> Style {
    // Cheap models green, mid yellow, expensive red — matches the
    // sysmon/agtop color language for "cost/danger" scaled quantities.
    match cost {
        Some(v) if v < 1.0 => Style::default().fg(Color::Green),
        Some(v) if v < 10.0 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::Red),
        None => Style::default().fg(Color::DarkGray),
    }
}

fn reasoning_glyph(on: bool) -> &'static str {
    if on { "★" } else { " " }
}

fn reasoning_style(on: bool) -> Style {
    if on {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default()
    }
}

/// Left-truncate `s` to `max` chars (not bytes) with a trailing ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_owned();
    }
    if max <= 1 {
        return "…".to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Absolute cell hit-test. Kept local (rather than shared with the
/// nearly-identical helper in `app.rs`) so this pane pulls in one
/// less cross-module dependency for four lines of code.
fn point_in_rect(col: u16, row: u16, r: Rect) -> bool {
    r.width > 0
        && r.height > 0
        && col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_smaller_than_total_centers_cursor() {
        // 100 rows, 10-row viewport, cursor at 50 → window ~[45, 55).
        let (s, e) = window_around(50, 100, 10);
        assert_eq!(e - s, 10);
        assert!(s <= 50 && 50 < e);
    }

    #[test]
    fn window_at_end_clamps_to_last_row() {
        let (s, e) = window_around(99, 100, 10);
        assert_eq!(e, 100);
        assert_eq!(s, 90);
    }

    #[test]
    fn window_when_data_fits_shows_all() {
        assert_eq!(window_around(3, 8, 10), (0, 8));
    }

    #[test]
    fn window_empty_returns_empty() {
        assert_eq!(window_around(0, 0, 10), (0, 0));
        assert_eq!(window_around(5, 100, 0), (0, 0));
    }

    #[test]
    fn column_widths_split_remaining_between_provider_and_model() {
        let (p, m) = column_widths(80);
        assert!(p >= 6);
        assert!(m >= 6);
        assert!(p + m + 21 >= 80 - 2, "sum roughly fills the inner width");
    }

    #[test]
    fn column_widths_hit_min_on_narrow_pane() {
        let (p, m) = column_widths(30);
        assert_eq!(p, 6);
        assert!(m >= 6);
    }

    #[test]
    fn column_widths_clamp_on_tiny_pane() {
        let (p, m) = column_widths(10);
        assert_eq!(p, 6);
        assert_eq!(m, 6);
    }

    #[test]
    fn truncate_at_ascii_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("helloworld", 5), "hell…");
        assert_eq!(truncate("x", 1), "x");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn format_title_stays_terse_on_narrow_pane() {
        let s = Snapshot {
            rows: vec![],
            provider_count: 87,
            last_error: None,
        };
        let t = format_title(&s, SortKey::InputCost, SortOrder::Descending, false);
        // Title fits inside a 60-col pane (models pane rarely gets more).
        assert!(t.chars().count() <= 60, "{t:?}");
        assert!(t.contains("sort:in$↓"));
        assert!(t.contains("87 providers"));
    }

    #[test]
    fn format_detail_line_includes_capabilities() {
        let row = ModelRow {
            provider_id: "openai".into(),
            provider_name: "OpenAI".into(),
            model_id: "gpt-4o".into(),
            model_name: "GPT-4o".into(),
            context: Some(128_000),
            output_limit: Some(16_384),
            input_cost: Some(2.5),
            output_cost: Some(10.0),
            reasoning: false,
            tool_call: true,
            attachment: true,
            family: None,
            release_date: Some("2024-05-13".into()),
            last_updated: None,
            is_text: true,
            open_weights: false,
        };
        let line = format_detail_line(&row);
        assert!(line.contains("openai/gpt-4o"));
        assert!(line.contains("128K ctx"));
        assert!(line.contains("$2.5/$10.0"));
        assert!(line.contains("tools"));
        assert!(line.contains("files"));
        assert!(line.contains("2024-05-13"));
    }

    #[test]
    fn cost_style_scales_by_price() {
        // Free tier / very cheap → green.
        assert_eq!(cost_style(Some(0.5)).fg, Some(Color::Green));
        // Mid → yellow.
        assert_eq!(cost_style(Some(5.0)).fg, Some(Color::Yellow));
        // Frontier → red.
        assert_eq!(cost_style(Some(75.0)).fg, Some(Color::Red));
        // Missing → dark grey.
        assert_eq!(cost_style(None).fg, Some(Color::DarkGray));
    }

    #[test]
    fn set_sort_flips_order_on_same_key() {
        let mut pane = build_test_pane();
        pane.sort_key = SortKey::Provider;
        pane.sort_order = SortOrder::Ascending;
        pane.set_sort(SortKey::Provider);
        assert_eq!(pane.sort_order, SortOrder::Descending);
    }

    #[test]
    fn set_sort_defaults_numeric_columns_descending() {
        let mut pane = build_test_pane();
        pane.set_sort(SortKey::Context);
        // Big-context first is what users want when they reach for `c`.
        assert_eq!(pane.sort_order, SortOrder::Descending);
    }

    #[test]
    fn set_sort_defaults_text_columns_ascending() {
        let mut pane = build_test_pane();
        pane.set_sort(SortKey::Name);
        assert_eq!(pane.sort_order, SortOrder::Ascending);
    }

    #[test]
    fn point_in_rect_matches_only_inside_cells() {
        let r = Rect::new(2, 3, 10, 5); // [2..12) × [3..8)
        assert!(point_in_rect(2, 3, r), "top-left corner is inside");
        assert!(point_in_rect(11, 7, r), "bottom-right cell is inside");
        assert!(!point_in_rect(12, 5, r), "just past right edge");
        assert!(!point_in_rect(5, 8, r), "just past bottom edge");
        assert!(!point_in_rect(1, 5, r), "left of the rect");
        assert!(!point_in_rect(5, 2, r), "above the rect");
    }

    #[test]
    fn point_in_rect_rejects_zero_sized() {
        // Empty rect (before first render) must never hit — otherwise
        // a stray click at (0,0) with `body_rect` = default would
        // hijack the wheel before the first paint captured a real rect.
        assert!(!point_in_rect(0, 0, Rect::default()));
    }

    #[test]
    fn move_cursor_clamps_to_row_range() {
        let mut pane = build_test_pane();
        // Seed a snapshot big enough to bump against; skip the worker.
        pane.snapshot = Snapshot {
            rows: (0..5)
                .map(|i| ModelRow {
                    provider_id: "p".into(),
                    provider_name: "P".into(),
                    model_id: format!("m{i}"),
                    model_name: format!("M{i}"),
                    context: None,
                    output_limit: None,
                    input_cost: None,
                    output_cost: None,
                    reasoning: false,
                    tool_call: false,
                    attachment: false,
                    family: None,
                    release_date: None,
                    last_updated: None,
                    is_text: true,
                    open_weights: false,
                })
                .collect(),
            provider_count: 1,
            last_error: None,
        };
        pane.move_cursor(100);
        assert_eq!(pane.cursor, 4, "clamped to last row");
        pane.move_cursor(-100);
        assert_eq!(pane.cursor, 0, "clamped to first row");
        pane.move_cursor(3);
        assert_eq!(pane.cursor, 3);
    }

    #[test]
    fn move_cursor_stays_zero_when_empty() {
        let mut pane = build_test_pane();
        // Default snapshot has zero rows — the wheel handler must
        // not underflow / panic.
        pane.move_cursor(WHEEL_STEP);
        assert_eq!(pane.cursor, 0);
        pane.move_cursor(-WHEEL_STEP);
        assert_eq!(pane.cursor, 0);
    }

    #[test]
    fn wheel_step_matches_pty_convention() {
        // pty_pane / fr_pane / agtop all use 3 rows-per-notch. Keep
        // the muscle memory identical.
        assert_eq!(WHEEL_STEP, 3);
    }

    /// Build a pane WITHOUT spawning the worker thread — direct field
    /// init since ModelsPane::new() hits the OS to spawn.
    fn build_test_pane() -> ModelsPane {
        // We still need a worker because it owns channels the pane
        // holds. The thread parks on recv immediately so this is cheap.
        ModelsPane::new()
    }
}
