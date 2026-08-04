//! Native AI-coding-agent status pane — a compact top-like table
//! showing every detected agent process (Claude Code, Codex, Aider,
//! Cursor, Gemini, Goose, …) with CPU%, RSS, uptime, project label,
//! model, tokens, cost, and dangerous-permission flag.
//!
//! Modelled on [`crate::sysmon_pane::SysmonPane`]: the pane owns an
//! [`AgtopWorker`] on a background OS thread that samples `sysinfo`
//! and enriches Claude sessions; the pane pulls a fresh
//! [`Snapshot`] every [`SAMPLE_INTERVAL`] and renders a filtered /
//! sorted view. `Enter` opens a per-agent detail popup with cost,
//! context-window fill, loaded skills / plugins, in-flight
//! subagents, tool counts, and a live-preview tail from the session
//! transcript.
//!
//! Keybindings:
//!
//! | Key             | Action                                    |
//! |-----------------|-------------------------------------------|
//! | `j / k / ↓ ↑`   | move cursor                               |
//! | `PgUp / PgDn`   | move by 10                                |
//! | `Home / End`    | first / last row                          |
//! | `c m t u a p s` | sort by cpu / mem / tokens / uptime / agent / pid / status |
//! | `S` (shift)     | smart sort                                |
//! | `Tab`           | flip sort direction                       |
//! | `/`             | enter filter mode                         |
//! | `Enter`         | open / close detail popup                 |
//! | `r` / `F5`      | force refresh                             |
//! | `Esc`           | dismiss modal (filter / detail)           |
//!
//! Attribution: detection regexes live in [`crate::agtop_matchers`];
//! session transcript reader + pricing table in
//! [`crate::agtop_session`] / [`crate::agtop_pricing`]. All three
//! are MIT-licensed ports of upstream `agtop` v2.4.24.
//!
//! [`AgtopWorker`]: crate::agtop_worker::AgtopWorker
//! [`Snapshot`]: crate::agtop_model::Snapshot

use std::any::Any;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use humansize::{DECIMAL, format_size};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, Widget, Wrap},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use crate::agtop_model::{
    AgentInfo, AgentStatus, AgentView, AgtopRequest, AgtopResponse, Snapshot, SortKey, SortOrder,
};
use crate::agtop_pricing::{CostBasis, format_cost};
use crate::agtop_worker::AgtopWorker;

/// Poll cadence. 1500 ms matches upstream `agtop`'s default
/// `--interval`.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

/// Modal state — either nothing, a live filter-entry, or the detail
/// popup pinned to a pid. Kill is deliberately absent (agents are
/// user-driven; the SysmonPane already offers `x` → `y` if the user
/// wants to terminate one).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Modal {
    #[default]
    None,
    Filter {
        input: String,
    },
    Detail {
        pid: u32,
        scroll: u16,
    },
}

/// Native `AgtopPane` provider — the "AI-agent top" for rimeterm.
pub struct AgtopPane {
    id: PaneId,
    title: String,
    worker: AgtopWorker,
    snapshot: Snapshot,
    /// Monotonic counter; bumped before every request so a late
    /// reply from a slow scan doesn't overwrite a fresher snapshot.
    requested_generation: u64,
    applied_generation: u64,
    last_request: Instant,
    sort_key: SortKey,
    sort_order: SortOrder,
    filter: Option<String>,
    cursor: usize,
    modal: Modal,
    /// Transient status line rendered at the bottom of the pane;
    /// cleared after the render pass that consumes it.
    hint: Option<String>,
}

impl AgtopPane {
    pub fn new() -> Self {
        let worker = AgtopWorker::spawn();
        let requested_generation = 1;
        worker.send(AgtopRequest::Snapshot {
            generation: requested_generation,
        });
        Self {
            id: PaneId::next(),
            title: "agtop".to_owned(),
            worker,
            snapshot: Snapshot::empty(),
            requested_generation,
            applied_generation: 0,
            last_request: Instant::now(),
            sort_key: SortKey::Smart,
            sort_order: SortOrder::Descending,
            filter: None,
            cursor: 0,
            modal: Modal::None,
            hint: None,
        }
    }

    fn agent_view(&self) -> AgentView {
        AgentView::from_snapshot(
            &self.snapshot,
            self.sort_key,
            self.sort_order,
            self.filter.as_deref(),
        )
    }

    fn request_snapshot(&mut self) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(AgtopRequest::Snapshot {
            generation: self.requested_generation,
        });
        self.last_request = Instant::now();
    }

    fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_order = self.sort_order.flip();
        } else {
            self.sort_key = key;
            self.sort_order = SortOrder::Descending;
        }
        self.cursor = 0;
    }

    fn set_hint<S: Into<String>>(&mut self, text: S) {
        self.hint = Some(text.into());
    }

    fn open_detail(&mut self) {
        let view = self.agent_view();
        if let Some(agent) = view.rows.get(self.cursor) {
            self.modal = Modal::Detail {
                pid: agent.pid,
                scroll: 0,
            };
        }
    }

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let sort = match self.sort_key {
            SortKey::Smart => "smart",
            SortKey::Cpu => "cpu",
            SortKey::Memory => "memory",
            SortKey::Tokens => "tokens",
            SortKey::Pid => "pid",
            SortKey::Label => "label",
            SortKey::Uptime => "uptime",
            SortKey::Status => "status",
        };
        let order = match self.sort_order {
            SortOrder::Ascending => "ascending",
            SortOrder::Descending => "descending",
        };
        let mut values = std::collections::BTreeMap::from([
            ("sort".into(), sort.into()),
            ("order".into(), order.into()),
        ]);
        if let Some(filter) = &self.filter {
            values.insert("filter".into(), filter.clone());
        }
        rimeterm_config::memory_state::PaneState { values }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        self.sort_key = match state.values.get("sort").map(String::as_str) {
            Some("cpu") => SortKey::Cpu,
            Some("memory") => SortKey::Memory,
            Some("tokens") => SortKey::Tokens,
            Some("pid") => SortKey::Pid,
            Some("label") => SortKey::Label,
            Some("uptime") => SortKey::Uptime,
            Some("status") => SortKey::Status,
            _ => SortKey::Smart,
        };
        self.sort_order = match state.values.get("order").map(String::as_str) {
            Some("ascending") => SortOrder::Ascending,
            _ => SortOrder::Descending,
        };
        self.filter = state
            .values
            .get("filter")
            .filter(|value| !value.is_empty())
            .cloned();
        self.cursor = 0;
        self.modal = Modal::None;
    }
}

impl Default for AgtopPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneProvider for AgtopPane {
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
        let title = format_title(&self.snapshot, self.sort_key, self.sort_order);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.height == 0 || inner.width == 0 {
            return RenderOutcome::default();
        }

        // Layout: [chips 1 line][table (fills)][optional 1-line footer].
        // The chips row is optional too — collapses if the pane is
        // ≤ 4 rows tall so a stacked mini-pane still shows agent rows.
        let has_footer = self.hint.is_some() || matches!(self.modal, Modal::Filter { .. });
        let want_chips = inner.height >= 5;
        let mut constraints: Vec<Constraint> = Vec::with_capacity(3);
        if want_chips {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Min(1));
        if has_footer {
            constraints.push(Constraint::Length(1));
        }
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);
        let mut idx = 0;
        let chips_rect = if want_chips {
            let r = split[idx];
            idx += 1;
            Some(r)
        } else {
            None
        };
        let body_rect = split[idx];
        idx += 1;
        let footer_rect = if has_footer { Some(split[idx]) } else { None };

        if let Some(rect) = chips_rect {
            render_chip_strip(frame, rect, &self.snapshot);
        }

        let view = self.agent_view();
        render_agent_table(frame, body_rect, &view, self.cursor);

        if let Some(rect) = footer_rect {
            render_footer(frame, rect, &self.modal, self.hint.as_deref());
        }

        // Detail popup overlays the pane's inner area. Draw last so
        // it wins the z-order.
        if let Modal::Detail { pid, scroll } = self.modal {
            if let Some(agent) = view.rows.iter().find(|a| a.pid == pid) {
                render_detail_popup(frame, inner, agent, scroll);
            } else {
                // Pid vanished between renders (agent exited) — drop
                // back to the list on the next key press.
                let msg = format!("pid {pid} no longer running (Esc to dismiss)");
                render_transient_overlay(frame, inner, &msg);
            }
        }

        // One-shot hints clear after the frame that displayed them.
        self.hint = None;

        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        // Global CONTROL / ALT combos aren't ours — let the app
        // route them (palette / tab switch / etc.).
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }

        // Take the modal out so the helper handlers get a clean
        // `&mut Modal` without needing to borrow through `self`.
        // Consumed → matched → written back at the end so the
        // pane's state stays intact on any early-return path.
        let mut modal = std::mem::take(&mut self.modal);
        let handled = match &mut modal {
            Modal::Filter { input } => {
                let mut next_modal = None;
                let out = on_key_filter(
                    input,
                    key.code,
                    &mut self.filter,
                    &mut self.cursor,
                    &mut next_modal,
                );
                if let Some(m) = next_modal {
                    modal = m;
                }
                out
            }
            Modal::Detail { scroll, .. } => {
                let mut next_modal = None;
                let out = on_key_detail(scroll, key.code, &mut next_modal);
                if let Some(m) = next_modal {
                    modal = m;
                }
                out
            }
            Modal::None => {
                self.modal = modal;
                return on_key_default(self, key);
            }
        };
        self.modal = modal;
        handled
    }

    fn reload(&mut self) {
        self.request_snapshot();
        self.set_hint("↻ refreshing");
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;

        // Fire the NEXT tick before draining so a slow reply doesn't
        // stack pending requests.
        if self.last_request.elapsed() >= SAMPLE_INTERVAL {
            self.request_snapshot();
        }

        for response in self.worker.drain() {
            let AgtopResponse::Snapshot {
                generation,
                snapshot,
            } = response;
            if generation < self.applied_generation {
                continue; // stale
            }
            self.applied_generation = generation;
            self.snapshot = snapshot;
            // Keep the cursor in-bounds if processes exited between
            // samples.
            let view = self.agent_view();
            if self.cursor >= view.rows.len() {
                self.cursor = view.rows.len().saturating_sub(1);
            }
            changed = true;
        }
        changed
    }
}

// ---------------------------------------------------------------------------
// Key handlers.
// ---------------------------------------------------------------------------

fn on_key_default(pane: &mut AgtopPane, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let view = pane.agent_view();
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
            let view = pane.agent_view();
            if !view.rows.is_empty() {
                pane.cursor = (pane.cursor + 10).min(view.rows.len() - 1);
            }
            true
        }
        KeyCode::PageUp => {
            pane.cursor = pane.cursor.saturating_sub(10);
            true
        }
        KeyCode::Home => {
            pane.cursor = 0;
            true
        }
        KeyCode::End => {
            let view = pane.agent_view();
            pane.cursor = view.rows.len().saturating_sub(1);
            true
        }
        KeyCode::Tab => {
            pane.sort_order = pane.sort_order.flip();
            true
        }
        KeyCode::Char('c') => {
            pane.set_sort(SortKey::Cpu);
            true
        }
        KeyCode::Char('m') => {
            pane.set_sort(SortKey::Memory);
            true
        }
        KeyCode::Char('t') => {
            pane.set_sort(SortKey::Tokens);
            true
        }
        KeyCode::Char('u') => {
            pane.set_sort(SortKey::Uptime);
            true
        }
        KeyCode::Char('a') => {
            pane.set_sort(SortKey::Label);
            true
        }
        KeyCode::Char('p') => {
            pane.set_sort(SortKey::Pid);
            true
        }
        KeyCode::Char('s') => {
            pane.set_sort(SortKey::Status);
            true
        }
        KeyCode::Char('S') => {
            pane.set_sort(SortKey::Smart);
            true
        }
        KeyCode::Char('/') => {
            pane.modal = Modal::Filter {
                input: pane.filter.clone().unwrap_or_default(),
            };
            true
        }
        KeyCode::Enter => {
            pane.open_detail();
            true
        }
        KeyCode::Char('r') | KeyCode::F(5) => {
            pane.request_snapshot();
            pane.set_hint("↻ refreshing");
            true
        }
        _ => false,
    }
}

fn on_key_filter(
    input: &mut String,
    code: KeyCode,
    filter: &mut Option<String>,
    cursor: &mut usize,
    next_modal: &mut Option<Modal>,
) -> bool {
    match code {
        KeyCode::Char(c) => {
            input.push(c);
            true
        }
        KeyCode::Backspace => {
            input.pop();
            true
        }
        KeyCode::Enter => {
            *filter = if input.is_empty() {
                None
            } else {
                Some(input.clone())
            };
            *cursor = 0;
            *next_modal = Some(Modal::None);
            true
        }
        KeyCode::Esc => {
            *next_modal = Some(Modal::None);
            true
        }
        _ => false,
    }
}

fn on_key_detail(scroll: &mut u16, code: KeyCode, next_modal: &mut Option<Modal>) -> bool {
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            *scroll = scroll.saturating_add(1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            *scroll = scroll.saturating_sub(1);
            true
        }
        KeyCode::PageDown => {
            *scroll = scroll.saturating_add(10);
            true
        }
        KeyCode::PageUp => {
            *scroll = scroll.saturating_sub(10);
            true
        }
        KeyCode::Char('g') | KeyCode::Home => {
            *scroll = 0;
            true
        }
        KeyCode::Char('G') | KeyCode::End => {
            // The paragraph widget clamps overscroll internally, so
            // a big number is safe here.
            *scroll = u16::MAX;
            true
        }
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            *next_modal = Some(Modal::None);
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Title / footer.
// ---------------------------------------------------------------------------

fn format_title(snapshot: &Snapshot, key: SortKey, order: SortOrder) -> String {
    format!(
        " agtop · {} agents · sort:{}{} ",
        snapshot.agent_count(),
        key.short(),
        order.arrow()
    )
}

/// Header chip strip — colored `LABEL VALUE` badges rendered inline,
/// mirroring upstream `agtop`'s `panels::draw_header`. Chips are
/// ordered by user priority: busy first (most actionable), then
/// financial context (cost / tokens), then load (cpu / mem), then
/// breakdown counts. Any chip whose value is zero is dropped so
/// the strip stays legible on a narrow pane.
fn render_chip_strip(frame: &mut Frame<'_>, area: Rect, snap: &Snapshot) {
    if area.width < 8 {
        return;
    }
    let busy = snap.count_status(AgentStatus::Busy) + snap.count_status(AgentStatus::Spawning);
    let active = snap.count_status(AgentStatus::Active);
    let waiting = snap.count_status(AgentStatus::Waiting);
    let done = snap.count_status(AgentStatus::Completed);
    let subagents: u32 = snap.agents.iter().map(|a| a.subagents).sum();
    let projects = {
        let mut seen = std::collections::HashSet::new();
        let mut count = 0usize;
        for a in &snap.agents {
            let key = if a.project.is_empty() {
                a.cwd.as_str()
            } else {
                a.project.as_str()
            };
            if !key.is_empty() && seen.insert(key.to_string()) {
                count += 1;
            }
        }
        count
    };

    // Chip = one bold colored VALUE span + a dim LABEL span. Every
    // chip pushes both plus a trailing space so consecutive chips
    // don't glue together.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(24);
    let push_chip = |spans: &mut Vec<Span<'static>>, label: &str, value: String, color: Color| {
        spans.push(Span::styled(
            format!(" {value} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{label} "),
            Style::default().fg(Color::DarkGray),
        ));
    };

    // agtop brand (bold white) + agent count.
    spans.push(Span::styled(
        " agtop ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    push_chip(
        &mut spans,
        "agents",
        snap.agent_count().to_string(),
        Color::White,
    );
    // Count chips ALWAYS render (matches upstream `agtop`'s
    // `panels::draw_header` — the strip has a stable rhythm the
    // user's eye learns). Even `0 busy` / `0 active` is useful info:
    // it's the difference between "everything's parked" and "the
    // pane hasn't loaded yet".
    push_chip(&mut spans, "busy", busy.to_string(), Color::Red);
    push_chip(&mut spans, "active", active.to_string(), Color::Yellow);
    push_chip(&mut spans, "waiting", waiting.to_string(), Color::LightBlue);
    push_chip(&mut spans, "done", done.to_string(), Color::Cyan);
    push_chip(&mut spans, "sub", subagents.to_string(), Color::LightRed);
    push_chip(&mut spans, "proj", projects.to_string(), Color::White);

    // Financial chips (`cost`, `tokens`) ARE conditional — a
    // brand-new session hasn't emitted `usage` blocks yet, and a
    // literal `$0.00 cost` next to a live agent would misread as
    // "this is free" rather than "we haven't seen a bill yet".
    // Upstream applies the same suppression rule.
    if snap.total_cost_usd > 0.0 {
        push_chip(
            &mut spans,
            "cost",
            format_cost(snap.total_cost_usd),
            Color::Rgb(240, 170, 80),
        );
    }
    if snap.total_tokens > 0 {
        push_chip(
            &mut spans,
            "tokens",
            format_tokens(snap.total_tokens),
            Color::Cyan,
        );
    }
    if snap.total_cpu >= 0.1 {
        push_chip(
            &mut spans,
            "cpu",
            format!("{:.0}%", snap.total_cpu),
            Color::LightGreen,
        );
    }
    if snap.total_rss > 0 {
        push_chip(
            &mut spans,
            "mem",
            format_size(snap.total_rss, DECIMAL),
            Color::LightMagenta,
        );
    }

    Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, modal: &Modal, hint: Option<&str>) {
    let (text, style) = match modal {
        Modal::Filter { input } => (format!("/ {input}_"), Style::default().fg(Color::Cyan)),
        _ => (
            hint.unwrap_or("").to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    };
    Paragraph::new(Line::styled(text, style)).render(area, frame.buffer_mut());
}

// ---------------------------------------------------------------------------
// Table.
// ---------------------------------------------------------------------------

fn render_agent_table(frame: &mut Frame<'_>, area: Rect, view: &AgentView, cursor: usize) {
    if view.rows.is_empty() {
        let msg = match view.filter.as_deref() {
            Some(f) if !f.is_empty() => format!("no agents match `{f}`"),
            _ => "no AI coding agents detected".to_owned(),
        };
        let hint = Paragraph::new(vec![
            Line::from(Span::styled(msg, Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(Span::styled(
                "start claude / codex / aider / cursor-agent / gemini / goose in a shell tab",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "keys: j/k move  · enter details · t tokens · c cpu · m mem · / filter",
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        hint.render(area, frame.buffer_mut());
        return;
    }

    let header = Row::new(vec![
        cell_dim(" "), // status glyph
        cell_dim(" "), // dangerous marker
        cell_dim("AGENT"),
        cell_dim("PID"),
        cell_dim(" CPU%"),
        cell_dim("MEM"),
        cell_dim("TOKENS"),
        cell_dim("UPTIME"),
        cell_dim("PROJECT"),
    ]);

    let project_width = project_col_width(area.width);

    let rows: Vec<Row> = view
        .rows
        .iter()
        .enumerate()
        .map(|(idx, agent)| render_row(idx == cursor, agent, project_width))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),                           // status glyph
            Constraint::Length(1),                           // dangerous marker
            Constraint::Length(13),                          // label (widest is "cursor-agent")
            Constraint::Length(6),                           // pid
            Constraint::Length(5),                           // cpu%
            Constraint::Length(9),                           // mem
            Constraint::Length(7),                           // tokens
            Constraint::Length(8),                           // uptime
            Constraint::Length(project_width.max(1) as u16), // project
        ],
    )
    .header(header)
    .column_spacing(1);
    ratatui::widgets::Widget::render(table, area, frame.buffer_mut());
}

fn render_row(selected: bool, agent: &AgentInfo, project_width: usize) -> Row<'static> {
    let base = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let status_style = status_color(agent.status).patch(base);
    let cpu_style = Style::default().fg(cpu_color(agent.cpu)).patch(base);
    let danger_style = Style::default()
        .fg(Color::Rgb(240, 170, 80))
        .add_modifier(Modifier::BOLD)
        .patch(base);
    let danger_cell = if agent.dangerous {
        cell("▍", danger_style)
    } else {
        cell(" ", base)
    };
    Row::new(vec![
        cell(agent.status.glyph(), status_style),
        danger_cell,
        cell(agent.label.clone(), base),
        cell(format!("{:>6}", agent.pid), base),
        cell(format!("{:>5.1}", agent.cpu), cpu_style),
        cell(format_size(agent.rss, DECIMAL), base),
        cell(format!("{:>7}", format_tokens(agent.tokens_total)), base),
        cell(format_uptime(agent.uptime_sec), base),
        cell(truncate(&project_or_task(agent), project_width), base),
    ])
}

/// Combine project + a compact indicator of what the agent is doing
/// right now — the "DOING" column upstream uses. For narrow panes
/// the PROJECT column swallows this whole; wider ones get the extra
/// context.
fn project_or_task(agent: &AgentInfo) -> String {
    match (agent.current_tool.as_deref(), agent.current_task.as_deref()) {
        (Some(tool), Some(task)) if !tool.is_empty() && !task.is_empty() => {
            // Prefix the project so filters still land — the raw
            // project label remains searchable in `filter`.
            let task_short: String = task.chars().take(48).collect();
            format!("{} · {tool}: {task_short}", agent.project)
        }
        (Some(tool), _) if !tool.is_empty() => format!("{} · {tool}", agent.project),
        (_, Some(task)) if !task.is_empty() => {
            let task_short: String = task.chars().take(64).collect();
            format!("{} · {task_short}", agent.project)
        }
        _ => agent.project.clone(),
    }
}

// ---------------------------------------------------------------------------
// Detail popup.
// ---------------------------------------------------------------------------

fn render_detail_popup(frame: &mut Frame<'_>, area: Rect, agent: &AgentInfo, scroll: u16) {
    // Popup fills ~90% × ~90% of the pane area, clamped to a
    // readable minimum. Kept inside the pane's rect so it doesn't
    // spill into adjacent panes / border.
    let width = ((area.width as u32 * 9 / 10) as u16)
        .max(50)
        .min(area.width);
    let height = ((area.height as u32 * 9 / 10) as u16)
        .max(10)
        .min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    let border_color = status_color(agent.status).fg.unwrap_or(Color::Cyan);
    let title = format!(
        " {} {}  pid {}  · {}  (esc to close) ",
        agent.status.glyph(),
        agent.label,
        agent.pid,
        display_project(agent)
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(popup);

    Clear.render(popup, frame.buffer_mut());
    block.render(popup, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    detail_lines(agent, inner.width as usize, &mut lines);

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    para.render(inner, frame.buffer_mut());
}

fn detail_lines(agent: &AgentInfo, width: usize, out: &mut Vec<Line<'static>>) {
    // Header line — status label + agent + pid + host details.
    let mut header = vec![
        Span::styled(
            format!(" {} {}  ", agent.status.glyph(), agent.status.label()),
            status_color(agent.status).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}  ", agent.label),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        dim(format!("pid {}", agent.pid)),
    ];
    if agent.dangerous {
        header.push(Span::raw("  "));
        header.push(Span::styled(
            format!(" {} ", agent.dangerous_flag),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(240, 170, 80))
                .add_modifier(Modifier::BOLD),
        ));
    }
    out.push(Line::from(header));
    out.push(Line::from(""));

    // Model / cost / basis.
    out.push(field(
        "model",
        agent
            .model
            .as_deref()
            .unwrap_or("(unknown — no assistant reply yet)"),
    ));
    let cost_span = match agent.cost_basis {
        CostBasis::Api => format!(
            "{}   ({})",
            format_cost(agent.cost_usd),
            agent.cost_basis.label()
        ),
        CostBasis::Local => "local (0.00 — runs on your hardware)".to_string(),
        CostBasis::Unknown => "unknown (no price-table match)".to_string(),
    };
    out.push(field("cost", &cost_span));

    // CPU / memory / uptime.
    out.push(field(
        "cpu",
        &format!(
            "{:.1}%   {}",
            agent.cpu,
            sparkline(&agent.cpu_history, 32.max(width.saturating_sub(18)))
        ),
    ));
    out.push(field("memory", &format_size(agent.rss, DECIMAL)));
    out.push(field(
        "uptime",
        &format_uptime_long(agent.uptime_sec, agent.session_started_ms),
    ));
    if !agent.ppid_name.is_empty() || agent.ppid > 0 {
        let launcher = if agent.ppid_name.is_empty() {
            format!("pid {}", agent.ppid)
        } else {
            format!("{} (pid {})", agent.ppid_name, agent.ppid)
        };
        out.push(field("launcher", &launcher));
    }

    // Tokens breakdown + cache.
    if agent.tokens_total > 0 {
        let in_uncached = agent
            .tokens_input
            .saturating_sub(agent.tokens_cache_read)
            .saturating_sub(agent.tokens_cache_write);
        out.push(field(
            "tokens",
            &format!(
                "{}   ({} in / {} out)",
                format_tokens(agent.tokens_total),
                format_tokens(agent.tokens_input),
                format_tokens(agent.tokens_output)
            ),
        ));
        if agent.tokens_cache_read > 0 || agent.tokens_cache_write > 0 {
            let cache_pct = if agent.tokens_input > 0 {
                (agent.tokens_cache_read as f64 / agent.tokens_input as f64) * 100.0
            } else {
                0.0
            };
            out.push(field(
                "cache",
                &format!(
                    "{:.0}% hit  ({} read / {} write / {} raw)",
                    cache_pct,
                    format_tokens(agent.tokens_cache_read),
                    format_tokens(agent.tokens_cache_write),
                    format_tokens(in_uncached),
                ),
            ));
        }
    }

    // Context-window fill bar.
    if agent.context_limit > 0 && agent.context_used > 0 {
        let bar_width = 24usize;
        let ratio = (agent.context_used as f64 / agent.context_limit as f64).min(1.0);
        let filled = (ratio * bar_width as f64) as usize;
        let bar_fg = if ratio >= 0.9 {
            Color::Red
        } else if ratio >= 0.7 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut ctx_line = Vec::new();
        ctx_line.push(dim("context  "));
        ctx_line.push(Span::styled(
            "█".repeat(filled),
            Style::default().fg(bar_fg),
        ));
        ctx_line.push(Span::styled(
            "░".repeat(bar_width.saturating_sub(filled)),
            Style::default().fg(Color::DarkGray),
        ));
        ctx_line.push(dim(format!(
            "  {:.0}%  ({} / {} tok)",
            ratio * 100.0,
            format_tokens(agent.context_used),
            format_tokens(agent.context_limit)
        )));
        out.push(Line::from(ctx_line));
        if ratio >= 0.9 {
            out.push(Line::from(vec![
                dim("         "),
                Span::styled(
                    "approaching auto-compaction",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    // Token-rate sparkline (only if we have real deltas).
    if agent.tokens_history.iter().any(|v| *v > 0.0) {
        let peak = agent.tokens_history.iter().cloned().fold(0.0_f64, f64::max);
        let avg = if agent.tokens_history.is_empty() {
            0.0
        } else {
            agent.tokens_history.iter().sum::<f64>() / agent.tokens_history.len() as f64
        };
        out.push(field(
            "rate",
            &format!(
                "{}  avg {} / tick · peak {}",
                sparkline(&agent.tokens_history, 32),
                format_tokens(avg as u64),
                format_tokens(peak as u64)
            ),
        ));
    }

    // Skills / plugins.
    if !agent.loaded_skills.is_empty() {
        out.push(field(
            "skills",
            &format!(
                "{} loaded — {}",
                agent.loaded_skills.len(),
                agent
                    .loaded_skills
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if !agent.loaded_plugins.is_empty() {
        out.push(field(
            "plugins",
            &format!(
                "{} enabled — {}",
                agent.loaded_plugins.len(),
                agent
                    .loaded_plugins
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }

    // Subagents.
    if agent.subagents > 0 {
        out.push(field(
            "subagents",
            &format!("{} in flight", agent.subagents),
        ));
        for descr in &agent.in_flight_subagents {
            out.push(Line::from(vec![
                dim("            "),
                Span::styled(format!("· {descr}"), Style::default().fg(Color::LightBlue)),
            ]));
        }
    }

    // Tool counts.
    if !agent.tool_counts.is_empty() {
        let joined = agent
            .tool_counts
            .iter()
            .take(6)
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" · ");
        out.push(field("tools", &joined));
    }

    // Session id.
    if let Some(id) = agent.session_id.as_deref() {
        if !id.is_empty() {
            let short: String = id.chars().take(24).collect();
            out.push(field("session", &short));
        }
    }

    out.push(Line::from(""));
    // Process / path facts.
    if !agent.exe.is_empty() {
        out.push(field("bin", &agent.exe));
    }
    if !agent.cwd.is_empty() {
        out.push(field("cwd", &agent.cwd));
    }
    if !agent.cmdline.is_empty() {
        let clip: String = agent.cmdline.chars().take(200).collect();
        out.push(field("cmd", &clip));
    }

    // Live preview from the session transcript tail.
    if !agent.recent_activity.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            "─ Live preview ────────────────────────────────────",
            Style::default().fg(Color::DarkGray),
        )));
        for line in &agent.recent_activity {
            let color = if line.starts_with('›') {
                Color::White
            } else if line.starts_with('→') {
                Color::LightYellow
            } else {
                Color::LightGreen
            };
            let clipped: String = line.chars().take(200).collect();
            out.push(Line::from(Span::styled(
                clipped,
                Style::default().fg(color),
            )));
        }
    }
}

fn render_transient_overlay(frame: &mut Frame<'_>, area: Rect, msg: &str) {
    let width = (area.width.saturating_sub(4)).min(60);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height / 2;
    let rect = Rect {
        x,
        y,
        width,
        height: 3,
    };
    Clear.render(rect, frame.buffer_mut());
    Paragraph::new(msg)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .render(rect, frame.buffer_mut());
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

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

fn dim<S: Into<String>>(text: S) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::DarkGray))
}

fn field(name: &'static str, value: &str) -> Line<'static> {
    let label = format!("{name:<10}");
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn status_color(status: AgentStatus) -> Style {
    match status {
        AgentStatus::Busy | AgentStatus::Spawning => Style::default().fg(Color::Red),
        AgentStatus::Active => Style::default().fg(Color::Yellow),
        AgentStatus::Idle => Style::default().fg(Color::Green),
        AgentStatus::Waiting => Style::default().fg(Color::LightBlue),
        AgentStatus::Completed => Style::default().fg(Color::Cyan),
        AgentStatus::Stale => Style::default().fg(Color::DarkGray),
    }
}

fn cpu_color(pct: f32) -> Color {
    if pct < 5.0 {
        Color::Green
    } else if pct < 30.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Compact human-readable uptime for the table.
fn format_uptime(secs: u64) -> String {
    if secs >= 86_400 {
        let days = secs / 86_400;
        let hours = (secs % 86_400) / 3600;
        format!("{days}d {hours}h")
    } else if secs >= 3600 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{hours}h {mins:02}m")
    } else if secs >= 60 {
        let mins = secs / 60;
        let s = secs % 60;
        format!("{mins}m {s:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Longer uptime for the detail popup — adds a `(resumed)` tag when
/// the session start diverges from process start by more than a
/// minute (the `--resume` case).
fn format_uptime_long(secs: u64, session_started_ms: u64) -> String {
    let base = format_uptime(secs);
    if session_started_ms == 0 {
        return base;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let session_age_sec = now.saturating_sub(session_started_ms) / 1000;
    if session_age_sec > secs.saturating_add(60) {
        format!(
            "{base}  ·  session {} (resumed)",
            format_uptime(session_age_sec)
        )
    } else {
        base
    }
}

/// Compact token count: `1.2k`, `4.5M`, `12M`, `1.2B`.
fn format_tokens(n: u64) -> String {
    if n == 0 {
        return "—".into();
    }
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return format!("{:.1}k", n as f64 / 1_000.0);
    }
    if n < 1_000_000_000 {
        return format!("{:.1}M", n as f64 / 1_000_000.0);
    }
    format!("{:.1}B", n as f64 / 1_000_000_000.0)
}

/// Braille sparkline over `samples`. Uses eight-level lower-half
/// blocks so the same char set works in every terminal without
/// requiring a Braille-capable font.
fn sparkline(samples: &[f64], max_cells: usize) -> String {
    if samples.is_empty() || max_cells == 0 {
        return String::new();
    }
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let take_from = samples.len().saturating_sub(max_cells);
    let slice = &samples[take_from..];
    let peak = slice.iter().cloned().fold(0.0_f64, f64::max);
    if peak <= 0.0 {
        return "▁".repeat(slice.len());
    }
    let mut out = String::with_capacity(slice.len());
    for v in slice {
        let idx = ((v.max(0.0) / peak) * (GLYPHS.len() as f64 - 1.0)).round() as usize;
        out.push(GLYPHS[idx.min(GLYPHS.len() - 1)]);
    }
    out
}

fn display_project(agent: &AgentInfo) -> String {
    if agent.cwd.is_empty() {
        agent.project.clone()
    } else {
        agent.cwd.clone()
    }
}

/// Compute a reasonable project-column width given the total inner
/// width. Reserves room for the fixed columns + separators; clamps
/// to at least 8 chars so the label never renders as a lone `…`.
fn project_col_width(inner_width: u16) -> usize {
    // Fixed constraints: 1 + 1 + 13 + 6 + 5 + 9 + 7 + 8 = 50 cells.
    // Add 8 column-separator cells = 58.
    const FIXED: u16 = 58;
    inner_width.saturating_sub(FIXED).max(8) as usize
}

/// Left-truncate `s` to `max` display chars with a trailing ellipsis.
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

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agtop_model::HISTORY_CAP;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn ctx() -> PaneRenderCtx<'static> {
        PaneRenderCtx {
            focused: false,
            title_override: None,
            focus_color: Color::LightBlue,
        }
    }

    fn agent(label: &str, pid: u32, cpu: f32, rss: u64, uptime: u64) -> AgentInfo {
        AgentInfo {
            label: label.into(),
            pid,
            cpu,
            rss,
            uptime_sec: uptime,
            cwd: String::new(),
            exe: String::new(),
            project: label.into(),
            cmdline: String::new(),
            ppid: 0,
            ppid_name: String::new(),
            status: AgentStatus::classify(cpu, None),
            dangerous_flag: String::new(),
            dangerous: false,
            model: None,
            session_id: None,
            session_started_ms: 0,
            current_tool: None,
            current_task: None,
            subagents: 0,
            in_flight_subagents: Vec::new(),
            recent_activity: Vec::new(),
            tokens_input: 0,
            tokens_output: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            tokens_total: 0,
            cost_usd: 0.0,
            cost_basis: CostBasis::Unknown,
            context_used: 0,
            context_limit: 200_000,
            tool_counts: Vec::new(),
            loaded_skills: Vec::new(),
            loaded_plugins: Vec::new(),
            cpu_history: Vec::new(),
            tokens_history: Vec::new(),
        }
    }

    fn seed_snapshot(pane: &mut AgtopPane, agents: Vec<AgentInfo>) {
        let total_cpu = agents.iter().map(|a| a.cpu).sum();
        let total_rss = agents.iter().map(|a| a.rss).sum();
        let total_tokens = agents.iter().map(|a| a.tokens_total).sum();
        let total_cost_usd = agents
            .iter()
            .filter(|a| a.cost_basis == CostBasis::Api)
            .map(|a| a.cost_usd)
            .sum();
        pane.snapshot = Snapshot {
            agents,
            total_cpu,
            total_rss,
            total_tokens,
            total_cost_usd,
            sampled_at: Instant::now(),
            sampled_at_ms: 0,
        };
    }

    fn buf_string(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().to_owned())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn new_pane_has_default_state() {
        let pane = AgtopPane::new();
        assert_eq!(pane.title, "agtop");
        assert_eq!(pane.sort_key, SortKey::Smart);
        assert_eq!(pane.sort_order, SortOrder::Descending);
        assert!(pane.filter.is_none());
    }

    #[test]
    fn render_with_no_agents_shows_empty_state_hint() {
        let mut pane = AgtopPane::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);
        assert!(rendered.contains("no AI coding agents detected"));
    }

    #[test]
    fn render_with_agents_shows_labels_and_tokens() {
        let mut pane = AgtopPane::new();
        let mut a = agent("claude", 111, 25.0, 128 * 1024 * 1024, 3660);
        a.tokens_total = 5_842_100;
        a.model = Some("claude-opus-4-7".into());
        a.cost_usd = 4.21;
        a.cost_basis = CostBasis::Api;
        seed_snapshot(
            &mut pane,
            vec![a, agent("codex", 222, 1.0, 64 * 1024 * 1024, 120)],
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);
        assert!(rendered.contains("agtop"));
        assert!(rendered.contains("claude"));
        assert!(rendered.contains("codex"));
        assert!(rendered.contains("111"));
        assert!(rendered.contains("222"));
        // Rounded 5.8M tokens.
        assert!(rendered.contains("5.8M"));
        // Cost in title.
        assert!(rendered.contains("$4.21") || rendered.contains("$4.2"));
    }

    #[test]
    fn tab_flips_sort_order() {
        let mut pane = AgtopPane::new();
        assert_eq!(pane.sort_order, SortOrder::Descending);
        assert!(pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert_eq!(pane.sort_order, SortOrder::Ascending);
        assert!(pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert_eq!(pane.sort_order, SortOrder::Descending);
    }

    #[test]
    fn m_selects_memory_sort() {
        let mut pane = AgtopPane::new();
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_key, SortKey::Memory);
    }

    #[test]
    fn t_selects_tokens_sort() {
        let mut pane = AgtopPane::new();
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_key, SortKey::Tokens);
    }

    #[test]
    fn capital_s_selects_smart_sort() {
        let mut pane = AgtopPane::new();
        // Move off smart first so we can verify the toggle sets it.
        pane.set_sort(SortKey::Cpu);
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_key, SortKey::Smart);
    }

    #[test]
    fn same_sort_key_second_press_flips_direction() {
        let mut pane = AgtopPane::new();
        // Cpu isn't default anymore — first press sets Cpu / Desc,
        // second press flips direction.
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_key, SortKey::Cpu);
        assert_eq!(pane.sort_order, SortOrder::Descending);
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_order, SortOrder::Ascending);
    }

    #[test]
    fn slash_opens_filter_modal() {
        let mut pane = AgtopPane::new();
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)));
        assert!(matches!(pane.modal, Modal::Filter { .. }));
    }

    #[test]
    fn filter_edit_commits_on_enter_clears_on_esc() {
        let mut pane = AgtopPane::new();
        pane.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "claude".chars() {
            pane.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(pane.filter.as_deref(), Some("claude"));
        assert!(matches!(pane.modal, Modal::None));

        pane.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(pane.filter.as_deref(), Some("claude"));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn enter_opens_detail_when_row_exists() {
        let mut pane = AgtopPane::new();
        seed_snapshot(&mut pane, vec![agent("claude", 42, 5.0, 0, 0)]);
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match pane.modal {
            Modal::Detail { pid, scroll } => {
                assert_eq!(pid, 42);
                assert_eq!(scroll, 0);
            }
            _ => panic!("expected Detail modal, got {:?}", pane.modal),
        }
    }

    #[test]
    fn enter_ignored_when_no_rows() {
        let mut pane = AgtopPane::new();
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn detail_scroll_j_advances_esc_closes() {
        let mut pane = AgtopPane::new();
        seed_snapshot(&mut pane, vec![agent("claude", 42, 5.0, 0, 0)]);
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        match pane.modal {
            Modal::Detail { scroll, .. } => assert_eq!(scroll, 2),
            _ => panic!("scroll lost"),
        }
        pane.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn cursor_j_and_k_navigate_within_bounds() {
        let mut pane = AgtopPane::new();
        seed_snapshot(
            &mut pane,
            vec![
                agent("claude", 1, 0.0, 0, 0),
                agent("codex", 2, 0.0, 0, 0),
                agent("aider", 3, 0.0, 0, 0),
            ],
        );
        assert_eq!(pane.cursor, 0);
        pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(pane.cursor, 1);
        pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(pane.cursor, 2);
        pane.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(pane.cursor, 1);
        // End jumps to the last row.
        pane.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(pane.cursor, 2);
        // Home returns to the top.
        pane.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(pane.cursor, 0);
    }

    #[test]
    fn control_modified_keys_pass_through() {
        let mut pane = AgtopPane::new();
        assert!(!pane.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn format_uptime_covers_all_ranges() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(65), "1m 05s");
        assert_eq!(format_uptime(3660), "1h 01m");
        assert_eq!(format_uptime(90_000), "1d 1h");
    }

    #[test]
    fn format_tokens_uses_readable_buckets() {
        assert_eq!(format_tokens(0), "—");
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(1500), "1.5k");
        assert_eq!(format_tokens(5_842_100), "5.8M");
        assert_eq!(format_tokens(2_500_000_000), "2.5B");
    }

    #[test]
    fn truncate_appends_ellipsis_and_respects_char_boundary() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 5), "abcd…");
        assert_eq!(truncate("abcdef", 1), "…");
        assert_eq!(truncate("abcdef", 0), "");
        assert_eq!(truncate("你好世界", 3), "你好…");
    }

    #[test]
    fn project_col_width_clamps_to_minimum() {
        assert_eq!(project_col_width(10), 8);
        assert_eq!(project_col_width(80), 22);
    }

    #[test]
    fn sparkline_generates_glyph_per_sample() {
        assert_eq!(sparkline(&[], 10), "");
        // All zero → floor glyph.
        assert_eq!(sparkline(&[0.0, 0.0, 0.0], 10), "▁▁▁");
        // Monotonically increasing hits the top glyph at the peak.
        let s = sparkline(&[1.0, 2.0, 3.0, 4.0], 10);
        assert!(s.ends_with('█'));
        assert_eq!(s.chars().count(), 4);
    }

    #[test]
    fn sparkline_respects_max_cells_by_taking_tail() {
        // Fill a history buffer and confirm only the tail renders.
        let mut samples: Vec<f64> = (0..HISTORY_CAP).map(|i| i as f64).collect();
        let s = sparkline(&samples, 4);
        assert_eq!(s.chars().count(), 4);
        // Add one more; still only 4 cells emitted.
        samples.push(HISTORY_CAP as f64);
        let s = sparkline(&samples, 4);
        assert_eq!(s.chars().count(), 4);
    }

    #[test]
    fn dangerous_row_renders_marker() {
        let mut pane = AgtopPane::new();
        let mut a = agent("claude", 42, 1.0, 1024, 60);
        a.dangerous = true;
        a.dangerous_flag = "--yolo".into();
        seed_snapshot(&mut pane, vec![a]);
        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);
        assert!(rendered.contains('▍'));
    }

    #[test]
    fn detail_popup_shows_model_and_cost() {
        let mut pane = AgtopPane::new();
        let mut a = agent("claude", 42, 1.0, 1024, 60);
        a.model = Some("claude-sonnet-4-7".into());
        a.cost_basis = CostBasis::Api;
        a.cost_usd = 1.23;
        a.tokens_input = 1_000_000;
        a.tokens_output = 5000;
        a.tokens_total = 1_005_000;
        a.context_used = 100_000;
        a.context_limit = 200_000;
        seed_snapshot(&mut pane, vec![a]);
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);
        assert!(
            rendered.contains("claude-sonnet-4-7"),
            "model missing: {rendered}"
        );
        assert!(rendered.contains("$1.23"), "cost missing: {rendered}");
        assert!(
            rendered.contains("context"),
            "context row missing: {rendered}"
        );
        assert!(rendered.contains("50%"), "context pct missing: {rendered}");
    }

    #[test]
    fn detail_popup_shows_recent_activity() {
        let mut pane = AgtopPane::new();
        let mut a = agent("claude", 42, 1.0, 1024, 60);
        a.model = Some("claude-sonnet-4-7".into());
        a.recent_activity = vec![
            "› thinking out loud".to_string(),
            "→ Bash: ls".to_string(),
            "← ok".to_string(),
        ];
        seed_snapshot(&mut pane, vec![a]);
        pane.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);
        assert!(rendered.contains("Live preview"));
        assert!(rendered.contains("Bash: ls"));
    }

    #[test]
    fn chip_strip_renders_busy_cost_tokens_when_present() {
        let mut pane = AgtopPane::new();
        let mut a = agent("claude", 42, 12.0, 128 * 1024 * 1024, 60);
        a.status = AgentStatus::Busy;
        a.tokens_total = 5_800_000;
        a.model = Some("claude-opus-4-7".into());
        a.cost_usd = 4.21;
        a.cost_basis = CostBasis::Api;
        seed_snapshot(&mut pane, vec![a]);

        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);

        assert!(rendered.contains("agtop"), "brand chip missing: {rendered}");
        assert!(
            rendered.contains("agents"),
            "agents chip missing: {rendered}"
        );
        assert!(rendered.contains("busy"), "busy chip missing: {rendered}");
        assert!(
            rendered.contains("$4.21") || rendered.contains("$4.2"),
            "cost chip missing: {rendered}"
        );
        assert!(
            rendered.contains("tokens"),
            "tokens chip missing: {rendered}"
        );
        assert!(rendered.contains("5.8M"), "token value missing: {rendered}");
    }

    #[test]
    fn chip_strip_hides_zero_valued_chips() {
        let mut pane = AgtopPane::new();
        let a = agent("claude", 1, 0.0, 1024, 60);
        seed_snapshot(&mut pane, vec![a]);

        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let rendered = buf_string(&terminal);

        assert!(rendered.contains("agtop"));
        assert!(rendered.contains("agents"));
        assert!(!rendered.contains("cost"), "cost chip leaked: {rendered}");
        // "tokens" appears in the TOKENS column header too, so we
        // only assert the value doesn't render (dash placeholder).
        assert!(!rendered.contains("$"), "cost value leaked: {rendered}");
    }
    #[test]
    fn stable_state_round_trips_without_cursor_or_detail() {
        let mut source = AgtopPane::new();
        source.sort_key = SortKey::Tokens;
        source.sort_order = SortOrder::Ascending;
        source.filter = Some("claude".into());
        source.cursor = 4;
        let state = source.snapshot_state();

        let mut restored = AgtopPane::new();
        restored.restore_state(&state);

        assert_eq!(restored.sort_key, SortKey::Tokens);
        assert_eq!(restored.sort_order, SortOrder::Ascending);
        assert_eq!(restored.filter.as_deref(), Some("claude"));
        assert_eq!(restored.cursor, 0);
        assert_eq!(restored.modal, Modal::None);
    }
}
