//! Native AI-coding-agent status pane — a compact top-like table showing
//! every detected agent process (Claude Code, Codex, Aider, Cursor,
//! Gemini, Goose, …) with CPU%, RSS, uptime, and project label.
//!
//! Modelled on [`crate::sysmon_pane::SysmonPane`]: the pane owns an
//! [`AgtopWorker`] on a background OS thread that samples `sysinfo`;
//! the pane pulls a fresh [`Snapshot`] every [`SAMPLE_INTERVAL`] and
//! renders a filtered / sorted view. Keybindings mirror the sysmon
//! process table so the muscle memory carries over:
//!
//! | Key             | Action                                    |
//! |-----------------|-------------------------------------------|
//! | `j / k / ↓ ↑`   | move cursor                               |
//! | `c m u a p s`   | sort by cpu / mem / uptime / agent / pid / status |
//! | `Tab`           | flip sort direction                       |
//! | `/`             | enter filter mode (numeric = pid, else label / project / cmdline) |
//! | `Enter`         | commit filter                             |
//! | `Esc`           | dismiss filter                            |
//! | `r` / `F5`      | force an immediate refresh                |
//!
//! Attribution: detection regexes live in [`crate::agtop_matchers`] and
//! are a direct MIT-licensed port of the upstream `agtop` binary's
//! matcher table.
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
    widgets::{Block, Borders, Paragraph, Row, Table, Widget},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use crate::agtop_model::{
    AgentStatus, AgentView, AgtopRequest, AgtopResponse, Snapshot, SortKey, SortOrder,
};
use crate::agtop_worker::AgtopWorker;

/// Poll cadence. 1500 ms matches upstream `agtop`'s default
/// `--interval`: agent processes don't churn faster than that in
/// practice, and slower ticks keep sysinfo's Windows backend from
/// stealing perceptible cycles.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

/// Modal state — no modal or filter-entry mode. No kill affordance
/// (agents are user-driven long-running processes; killing them
/// belongs in the SysmonPane).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Modal {
    #[default]
    None,
    Filter {
        input: String,
    },
}

/// Native `AgtopPane` provider — the "AI-agent top" for rimeterm.
pub struct AgtopPane {
    id: PaneId,
    title: String,
    worker: AgtopWorker,
    snapshot: Snapshot,
    /// Monotonic counter; bumped before every `Snapshot` request so
    /// late replies (worker delayed by a slow scan) land harmlessly
    /// when they overlap a fresher one.
    requested_generation: u64,
    applied_generation: u64,
    /// When the pane last kicked the worker; drives the tick loop.
    last_request: Instant,
    sort_key: SortKey,
    sort_order: SortOrder,
    filter: Option<String>,
    cursor: usize,
    modal: Modal,
    /// Transient status text rendered on the footer row. Cleared
    /// after the next render pass consumes it.
    hint: Option<String>,
}

impl AgtopPane {
    pub fn new() -> Self {
        let worker = AgtopWorker::spawn();
        // Prime the counter with a first sample immediately so the
        // pane never renders an empty "no data" state on startup.
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
            sort_key: SortKey::Cpu,
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

        // Bottom row: hint / filter footer whenever there's something
        // to show. Rest of the inner rect goes to the table.
        let hint_active = self.hint.is_some() || !matches!(self.modal, Modal::None);
        let (body_rect, footer_rect) = if hint_active && inner.height >= 2 {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(inner);
            (split[0], Some(split[1]))
        } else {
            (inner, None)
        };

        let view = self.agent_view();
        render_agent_table(frame, body_rect, &view, self.cursor);

        if let Some(rect) = footer_rect {
            render_footer(frame, rect, &self.modal, self.hint.as_deref());
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

        // Filter modal owns arbitrary chars; the default handler only
        // sees keys when no modal is up.
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

    fn reload(&mut self) {
        self.request_snapshot();
        self.set_hint("↻ refreshing");
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;

        // Fire the NEXT tick before draining so a slow reply doesn't
        // stack multiple pending requests.
        if self.last_request.elapsed() >= SAMPLE_INTERVAL {
            self.request_snapshot();
        }

        for response in self.worker.drain() {
            let AgtopResponse::Snapshot {
                generation,
                snapshot,
            } = response;
            if generation < self.applied_generation {
                // Stale — a fresher snapshot has already been
                // applied. Discard.
                continue;
            }
            self.applied_generation = generation;
            self.snapshot = snapshot;
            // Keep the cursor in-bounds after processes exit between
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
        KeyCode::Char('/') => {
            pane.modal = Modal::Filter {
                input: pane.filter.clone().unwrap_or_default(),
            };
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

/// Compose the tab-title-plus-summary shown in the border. Kept small
/// so it doesn't push the sort indicator past the border on a narrow
/// pane.
fn format_title(snapshot: &Snapshot, key: SortKey, order: SortOrder) -> String {
    let arrow = match order {
        SortOrder::Ascending => "↑",
        SortOrder::Descending => "↓",
    };
    let key_name = match key {
        SortKey::Cpu => "cpu",
        SortKey::Memory => "mem",
        SortKey::Pid => "pid",
        SortKey::Label => "agent",
        SortKey::Uptime => "uptime",
        SortKey::Status => "status",
    };
    format!(
        " agtop · {} agents · sort:{}{} ",
        snapshot.agents.len(),
        key_name,
        arrow
    )
}

/// Draw the main agents table with a highlighted cursor row and an
/// empty-state hint when nothing matches.
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
        ]);
        hint.render(area, frame.buffer_mut());
        return;
    }

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("AGENT"),
        cell_dim("PID"),
        cell_dim(" CPU%"),
        cell_dim("MEM"),
        cell_dim("UPTIME"),
        cell_dim("PROJECT"),
    ]);

    // Truncate project labels so a monorepo path doesn't blow out the
    // last column on a narrow pane.
    let project_width = project_col_width(area.width);

    let rows: Vec<Row> = view
        .rows
        .iter()
        .enumerate()
        .map(|(idx, agent)| {
            let selected = idx == cursor;
            let status_style = status_color(agent.status);
            let cpu_style = Style::default().fg(cpu_color(agent.cpu));
            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                cell(agent.status.glyph(), status_style.patch(base)),
                cell(&agent.label, base),
                cell(format!("{:>6}", agent.pid), base),
                cell(format!("{:>5.1}", agent.cpu), cpu_style.patch(base)),
                cell(format_size(agent.rss, DECIMAL), base),
                cell(format_uptime(agent.uptime_sec), base),
                cell(truncate(&agent.project, project_width), base),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),                           // status glyph
            Constraint::Length(13),                          // label (widest is "cursor-agent")
            Constraint::Length(6),                           // pid
            Constraint::Length(5),                           // cpu%
            Constraint::Length(9),                           // mem (e.g. "128.4 MB")
            Constraint::Length(8),                           // uptime
            Constraint::Length(project_width.max(1) as u16), // project
        ],
    )
    .header(header)
    .column_spacing(1);
    ratatui::widgets::Widget::render(table, area, frame.buffer_mut());
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, modal: &Modal, hint: Option<&str>) {
    let (text, style) = match modal {
        Modal::Filter { input } => (format!("/ {input}_"), Style::default().fg(Color::Cyan)),
        Modal::None => (
            hint.unwrap_or("").to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    };
    Paragraph::new(Line::styled(text, style)).render(area, frame.buffer_mut());
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

fn status_color(status: AgentStatus) -> Style {
    match status {
        AgentStatus::Busy => Style::default().fg(Color::Red),
        AgentStatus::Active => Style::default().fg(Color::Yellow),
        AgentStatus::Idle => Style::default().fg(Color::Green),
        AgentStatus::Sleeping => Style::default().fg(Color::DarkGray),
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

/// Compact human-readable uptime. Matches the shape SysmonPane's
/// system column uses so both left-bottom tabs look coherent.
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

/// Compute a reasonable project-column width given the total inner
/// width. Reserves room for the fixed columns + separators; clamps to
/// at least 8 chars so the label never renders as a lone `…`.
fn project_col_width(inner_width: u16) -> usize {
    // Fixed constraints: 1 + 13 + 6 + 5 + 9 + 8 = 42 cells.  Add
    // 6 column-separator cells = 48.
    const FIXED: u16 = 48;
    inner_width.saturating_sub(FIXED).max(8) as usize
}

/// Left-truncate `s` to `max` chars (not bytes) with a trailing ellipsis.
/// Cheap enough to call per row per frame.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agtop_model::AgentInfo;
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
            project: label.into(),
            cmdline: String::new(),
            status: if cpu >= 5.0 {
                AgentStatus::Busy
            } else if cpu >= 0.5 {
                AgentStatus::Active
            } else {
                AgentStatus::Idle
            },
        }
    }

    /// Feed the pane a synthetic snapshot without going through the
    /// worker so we can assert on rendering deterministically.
    fn seed_snapshot(pane: &mut AgtopPane, agents: Vec<AgentInfo>) {
        let total_cpu = agents.iter().map(|a| a.cpu).sum();
        let total_rss = agents.iter().map(|a| a.rss).sum();
        pane.snapshot = Snapshot {
            agents,
            total_cpu,
            total_rss,
            sampled_at: Instant::now(),
        };
    }

    #[test]
    fn new_pane_has_default_state() {
        let pane = AgtopPane::new();
        assert_eq!(pane.title, "agtop");
        assert_eq!(pane.sort_key, SortKey::Cpu);
        assert_eq!(pane.sort_order, SortOrder::Descending);
        assert!(pane.filter.is_none());
    }

    #[test]
    fn render_with_no_agents_shows_empty_state_hint() {
        let mut pane = AgtopPane::new();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let rendered: String = (0..buf.area.height)
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
            .join("\n");
        assert!(rendered.contains("no AI coding agents detected"));
    }

    #[test]
    fn render_with_agents_shows_labels() {
        let mut pane = AgtopPane::new();
        seed_snapshot(
            &mut pane,
            vec![
                agent("claude", 111, 25.0, 128 * 1024 * 1024, 3660),
                agent("codex", 222, 1.0, 64 * 1024 * 1024, 120),
            ],
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(area, frame, &ctx());
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let rendered: String = (0..buf.area.height)
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
            .join("\n");
        assert!(rendered.contains("agtop"));
        assert!(rendered.contains("claude"));
        assert!(rendered.contains("codex"));
        assert!(rendered.contains("111"));
        assert!(rendered.contains("222"));
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
    fn same_sort_key_second_press_flips_direction() {
        let mut pane = AgtopPane::new();
        // cpu is the default; pressing `c` should flip direction.
        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert_eq!(pane.sort_key, SortKey::Cpu);
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

        // Esc from a fresh filter modal cancels without touching the
        // committed filter.
        pane.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        pane.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(pane.filter.as_deref(), Some("claude"));
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
        // Cursor stops at last row rather than overflowing.
        assert_eq!(pane.cursor, 2);
        pane.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(pane.cursor, 1);
    }

    #[test]
    fn control_modified_keys_pass_through() {
        // Ctrl+/ etc. must be forwarded to the app so palette
        // shortcuts still work when this pane is focused.
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
    fn truncate_appends_ellipsis_and_respects_char_boundary() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 5), "abcd…");
        assert_eq!(truncate("abcdef", 1), "…");
        assert_eq!(truncate("abcdef", 0), "");
        // Multi-byte chars count as one column, not their byte width.
        assert_eq!(truncate("你好世界", 3), "你好…");
    }

    #[test]
    fn project_col_width_clamps_to_minimum() {
        // Even at a laughably narrow pane, project stays >= 8.
        assert_eq!(project_col_width(10), 8);
        // Comfortable widths give us plenty of room.
        assert_eq!(project_col_width(80), 32);
    }
}
