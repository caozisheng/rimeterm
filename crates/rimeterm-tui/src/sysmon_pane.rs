//! Native system-monitor pane.
//!
//! Read-only workspace panel backed by [`SysmonWorker`]. Overview view
//! is a big Braille-marker CPU waveform + memory/swap gauges + optional
//! GPU / Docker / cgroup rows; Processes view is a sortable, filterable
//! table. Sampling is worker-driven: the pane sends a `Snapshot`
//! request every 200 ms via [`SysmonPane::poll_background`].
//!
//! Key bindings match the design doc §4.3:
//!
//! | Key           | Action                                         |
//! |---------------|------------------------------------------------|
//! | `Tab`         | cycle view (Overview → Processes → Overview)  |
//! | `j / k / ↓ ↑` | move cursor in the process table               |
//! | `c m p n`     | sort by cpu / memory / pid / name              |
//! | `/`           | enter filter mode (numeric = pid, else name)   |
//! | `Enter`       | apply / commit filter                          |
//! | `Esc`         | dismiss filter mode or kill-confirm prompt     |
//! | `x`           | raise kill-confirm prompt for the selected row |
//! | `y`           | confirm kill                                   |

use std::any::Any;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use humansize::{DECIMAL, format_size};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, List, ListItem, Paragraph, Widget},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

use crate::sysmon_model::{
    DockerStats, GpuStats, ProcessView, Snapshot, SortKey, SortOrder, SysmonRequest,
    SysmonResponse, SysmonView,
};
use crate::sysmon_worker::SysmonWorker;

/// Worker-poll cadence. 200 ms keeps the wave smooth without stealing
/// idle-CPU headroom; a full process-table sample on a mid-tier laptop
/// finishes well inside 30 ms.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// How many CPU-average samples the chart retains. 300 samples × 200 ms
/// tick ≈ 60 s of history, wide enough to spot a build spike without
/// stealing more than one screen-height of state.
const CPU_HISTORY_CAP: usize = 300;

/// Modal state — nothing, filter entry, or kill-confirm prompt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Modal {
    #[default]
    None,
    Filter {
        input: String,
    },
    ConfirmKill {
        pid: u32,
        name: String,
    },
}

/// Native SysmonPane provider.
pub struct SysmonPane {
    id: PaneId,
    title: String,
    worker: SysmonWorker,
    snapshot: Snapshot,
    /// Monotonic counter; the pane bumps it before each `Snapshot`
    /// request so stale replies (worker delayed by a slow tick) land
    /// harmlessly when they overlap a fresh one.
    requested_generation: u64,
    applied_generation: u64,
    /// When the pane last kicked the worker; drives 200 ms polling.
    last_request: Instant,
    view: SysmonView,
    sort_key: SortKey,
    sort_order: SortOrder,
    filter: Option<String>,
    process_cursor: usize,
    /// Rolling CPU-average history for the Overview chart. Kept as f64
    /// so it drops straight into `Dataset::data` without an extra copy.
    cpu_history: Vec<f64>,
    modal: Modal,
    /// Transient status line rendered at the bottom of the pane.
    /// Cleared after the pane's next render pass consumes it.
    hint: Option<String>,
}

impl SysmonPane {
    pub fn new() -> Self {
        let worker = SysmonWorker::spawn();
        // Kick off the first sample immediately so the pane doesn't
        // show empty state for a whole tick.
        let requested_generation = 1;
        worker.send(SysmonRequest::Snapshot {
            generation: requested_generation,
        });
        Self {
            id: PaneId::next(),
            title: "Sysmon".to_owned(),
            worker,
            snapshot: Snapshot::empty(),
            requested_generation,
            applied_generation: 0,
            // Backdate so the tick loop schedules the NEXT sample at
            // roughly `now + SAMPLE_INTERVAL` after the seed reply lands.
            last_request: Instant::now(),
            view: SysmonView::Overview,
            sort_key: SortKey::Cpu,
            sort_order: SortOrder::Descending,
            filter: None,
            process_cursor: 0,
            cpu_history: Vec::with_capacity(CPU_HISTORY_CAP),
            modal: Modal::None,
            hint: None,
        }
    }

    fn process_view(&self) -> ProcessView {
        ProcessView::from_snapshot(
            &self.snapshot,
            self.sort_key,
            self.sort_order,
            self.filter.as_deref(),
        )
    }

    fn selected_process(&self) -> Option<crate::sysmon_model::ProcessInfo> {
        let view = self.process_view();
        view.rows.get(self.process_cursor).cloned()
    }

    fn request_snapshot(&mut self) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(SysmonRequest::Snapshot {
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
        self.process_cursor = 0;
    }

    fn set_hint<S: Into<String>>(&mut self, text: S) {
        self.hint = Some(text.into());
    }
}

impl Default for SysmonPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneProvider for SysmonPane {
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
        let title = sysmon_title(self.view, &self.snapshot);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.height == 0 || inner.width == 0 {
            return RenderOutcome::default();
        }

        // Reserve one row for the hint / filter bar at the bottom when
        // one is active, otherwise the whole inner rect is body.
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

        match self.view {
            SysmonView::Overview => {
                render_overview(frame, body_rect, &self.snapshot, &self.cpu_history)
            }
            SysmonView::Processes => render_processes_view(
                frame,
                body_rect,
                &self.snapshot,
                &self.cpu_history,
                &self.process_view(),
                self.process_cursor,
            ),
        }

        if let Some(rect) = footer_rect {
            render_footer(frame, rect, &self.modal, self.hint.as_deref());
        }
        // Hint is single-frame; clear it now that the next render will
        // reflect whatever fresh state produced it. Modals persist
        // until the user dismisses them explicitly.
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

        // Modal keys take precedence — the filter editor consumes
        // arbitrary chars, and the kill-confirm prompt only accepts y/n.
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
                    self.process_cursor = 0;
                    self.modal = Modal::None;
                    true
                }
                KeyCode::Esc => {
                    self.modal = Modal::None;
                    true
                }
                _ => false,
            },
            Modal::ConfirmKill { pid, name } => {
                let pid = *pid;
                let name = name.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.worker.send(SysmonRequest::Kill { pid });
                        self.set_hint(format!("kill sent: {name} (pid {pid})"));
                        self.modal = Modal::None;
                        true
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.modal = Modal::None;
                        true
                    }
                    _ => false,
                }
            }
            Modal::None => on_key_default(self, key),
        }
    }

    fn reload(&mut self) {
        self.request_snapshot();
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;

        // Fire the next tick BEFORE draining, so a slow worker reply
        // doesn't stack multiple pending requests: `request_snapshot`
        // resets `last_request`, and the drain below still lands the
        // fresh reply before render.
        if self.last_request.elapsed() >= SAMPLE_INTERVAL {
            self.request_snapshot();
        }

        for response in self.worker.drain() {
            match response {
                SysmonResponse::Snapshot(snap) => {
                    if snap.generation < self.applied_generation {
                        continue;
                    }
                    self.applied_generation = snap.generation;
                    // Retain the pre-snapshot cpu_avg in history BEFORE
                    // overwriting the snapshot, so the chart shows a
                    // trailing curve even during rapid state churn.
                    self.cpu_history.push(snap.cpu_avg as f64);
                    if self.cpu_history.len() > CPU_HISTORY_CAP {
                        let overflow = self.cpu_history.len() - CPU_HISTORY_CAP;
                        self.cpu_history.drain(..overflow);
                    }
                    self.snapshot = snap;
                    // Keep the process cursor in-bounds when the row
                    // count shrinks (pids exited between samples).
                    let view = self.process_view();
                    if self.process_cursor >= view.rows.len() {
                        self.process_cursor = view.rows.len().saturating_sub(1);
                    }
                    changed = true;
                }
                SysmonResponse::KillResult { pid, success } => {
                    if success {
                        self.set_hint(format!("kill ok: pid {pid}"));
                    } else {
                        self.set_hint(format!("kill failed: pid {pid} (permission or gone)"));
                    }
                    // Force an immediate refresh so the killed pid
                    // drops off the table before the next tick.
                    self.request_snapshot();
                    changed = true;
                }
            }
        }
        changed
    }
}

fn on_key_default(pane: &mut SysmonPane, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Tab => {
            pane.view = pane.view.cycle_next();
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if matches!(pane.view, SysmonView::Processes) {
                let view = pane.process_view();
                if !view.rows.is_empty() {
                    pane.process_cursor = (pane.process_cursor + 1).min(view.rows.len() - 1);
                }
                true
            } else {
                false
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if matches!(pane.view, SysmonView::Processes) {
                pane.process_cursor = pane.process_cursor.saturating_sub(1);
                true
            } else {
                false
            }
        }
        KeyCode::Char('c') => {
            pane.view = SysmonView::Processes;
            pane.set_sort(SortKey::Cpu);
            true
        }
        KeyCode::Char('m') => {
            pane.view = SysmonView::Processes;
            pane.set_sort(SortKey::Memory);
            true
        }
        KeyCode::Char('p') => {
            pane.view = SysmonView::Processes;
            pane.set_sort(SortKey::Pid);
            true
        }
        KeyCode::Char('n') => {
            pane.view = SysmonView::Processes;
            pane.set_sort(SortKey::Name);
            true
        }
        KeyCode::Char('/') => {
            pane.view = SysmonView::Processes;
            pane.modal = Modal::Filter {
                input: pane.filter.clone().unwrap_or_default(),
            };
            true
        }
        KeyCode::Char('x') => {
            if let Some(row) = pane.selected_process() {
                pane.modal = Modal::ConfirmKill {
                    pid: row.pid,
                    name: row.name,
                };
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn sysmon_title(view: SysmonView, snapshot: &Snapshot) -> String {
    let view_name = match view {
        SysmonView::Overview => "Overview",
        SysmonView::Processes => "Processes",
    };
    let cpu = format!("cpu {:>4.1}%", snapshot.cpu_avg);
    let mem = format!(
        "mem {}/{}",
        format_size(snapshot.memory.used, DECIMAL),
        format_size(snapshot.memory.total, DECIMAL),
    );
    format!(" Sysmon · {view_name} · {cpu} · {mem} ")
}

// ─── Overview view ──────────────────────────────────────────────────

fn render_overview(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot, cpu_history: &[f64]) {
    // Overview stacks three horizontal bands top-to-bottom, plus two
    // optional single-line rows for Docker and cgroup context:
    //
    //   1. CPU waveform                       — flex, min 6 rows
    //   2. GPU + Top Processes (side-by-side) — 12 rows
    //   3. Network + Disk + System (3 cols)   — 7 rows
    //   4. Docker row                         — 1 row (when daemon up)
    //   5. cgroup row                         — 1 row (when in container)
    //
    // Sections that don't fit silently drop off the bottom instead of
    // shrinking every neighbour into unreadable slivers. Memory / Swap
    // gauges retired — current-usage numbers still surface via the
    // pane's border title (`sysmon_title`).
    const CPU_MIN: u16 = 6;
    const GPU_PROCS_ROWS: u16 = 12;
    const NET_DISK_SYS_ROWS: u16 = 8;

    #[derive(Clone, Copy)]
    enum Section {
        Cpu,
        GpuAndProcs,
        NetDiskSys,
        Docker,
        Cgroup,
    }

    let mut constraints: Vec<Constraint> = vec![Constraint::Min(CPU_MIN)];
    let mut sections: Vec<Section> = vec![Section::Cpu];
    let mut used: u16 = CPU_MIN;
    let try_push = |rows: u16,
                    section: Section,
                    cs: &mut Vec<Constraint>,
                    ss: &mut Vec<Section>,
                    used: &mut u16| {
        if used.saturating_add(rows) <= area.height {
            cs.push(Constraint::Length(rows));
            ss.push(section);
            *used += rows;
        }
    };
    try_push(
        GPU_PROCS_ROWS,
        Section::GpuAndProcs,
        &mut constraints,
        &mut sections,
        &mut used,
    );
    try_push(
        NET_DISK_SYS_ROWS,
        Section::NetDiskSys,
        &mut constraints,
        &mut sections,
        &mut used,
    );
    if snapshot.docker.is_some() {
        try_push(
            1,
            Section::Docker,
            &mut constraints,
            &mut sections,
            &mut used,
        );
    }
    if snapshot.cgroup.as_ref().is_some_and(|c| c.is_container) {
        try_push(
            1,
            Section::Cgroup,
            &mut constraints,
            &mut sections,
            &mut used,
        );
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (rect, section) in rows.iter().zip(sections.iter()) {
        match section {
            Section::Cpu => render_cpu_chart(frame, *rect, snapshot, cpu_history),
            Section::GpuAndProcs => {
                // 40/60 split: GPU box on the left, Top Processes on
                // the right. Top Processes needs more room for its
                // 4-column layout (PID / CPU% / MEM / NAME).
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .split(*rect);
                render_gpu_box(frame, cols[0], snapshot);
                render_top_procs_mini(frame, cols[1], snapshot);
            }
            Section::NetDiskSys => {
                // Three equal columns. On very narrow panes each
                // column falls back to only what fits — the child
                // renderers already truncate their content.
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Ratio(1, 3),
                        Constraint::Ratio(1, 3),
                        Constraint::Ratio(1, 3),
                    ])
                    .split(*rect);
                render_network(frame, cols[0], snapshot);
                render_disk(frame, cols[1], snapshot);
                render_system(frame, cols[2], snapshot);
            }
            Section::Docker => {
                if let Some(docker) = &snapshot.docker {
                    render_docker_row(frame, *rect, docker);
                }
            }
            Section::Cgroup => {
                if let Some(cg) = &snapshot.cgroup {
                    render_cgroup_row(frame, *rect, cg);
                }
            }
        }
    }
}

fn render_cpu_chart(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot, cpu_history: &[f64]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(cpu_chart_title(snapshot));
    let inner_area = block.inner(area);
    if inner_area.height == 0 || inner_area.width == 0 {
        block.render(area, frame.buffer_mut());
        return;
    }

    // Convert the ring buffer of samples into an (x, y) series. `x`
    // is monotonic sample index so the wave scrolls from right to
    // left as new samples land; `y` is CPU percent.
    let data: Vec<(f64, f64)> = cpu_history
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f64, v.clamp(0.0, 100.0)))
        .collect();
    let x_max = (cpu_history.len().max(1) as f64) - 1.0;
    let x_bounds = [
        (x_max - (CPU_HISTORY_CAP as f64 - 1.0)).max(0.0),
        x_max.max(1.0),
    ];
    // The dataset name below drives ratatui's chart legend. We hide
    // the legend entirely (`legend_position(None)`) so the current
    // CPU value never shows up as an ambiguous swatch-with-no-number
    // pair; the live value lives in the block title instead.
    let datasets = vec![
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(cpu_color(snapshot.cpu_avg)))
            .data(&data),
    ];
    Chart::new(datasets)
        .block(block)
        .legend_position(None)
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(x_bounds),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, 100.0])
                .labels(vec![
                    Line::from("0%"),
                    Line::from("50%"),
                    Line::from("100%"),
                ]),
        )
        .render(area, frame.buffer_mut());
}

fn cpu_chart_title(snapshot: &Snapshot) -> String {
    // Live CPU value goes in the title so it's visible even when the
    // chart legend is disabled (see `render_cpu_chart`). Format
    // deliberately drops the `avg` prefix on narrow panes where every
    // char counts — the "%" makes it unambiguous.
    format!(
        " CPU · {} cores · {:.1}% ",
        snapshot.cpu_per_core.len(),
        snapshot.cpu_avg
    )
}

fn render_gpu_box(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot) {
    // No GPU → single bordered box with a placeholder line so the
    // layout column stays predictable even on hosts without a driver.
    if snapshot.gpus.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" GPU ");
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        Paragraph::new(Line::styled(
            "no NVIDIA GPU detected",
            Style::default().fg(Color::DarkGray),
        ))
        .render(inner, frame.buffer_mut());
        return;
    }

    // One bordered sub-block per GPU, titled `GPU (N)` (1-indexed to
    // match how humans think about "first GPU / second GPU"). The
    // column area splits evenly among them via `Constraint::Ratio`;
    // for very short columns a sub-block may only get 2 rows and its
    // content clips naturally — better than dropping GPUs off screen.
    let n = snapshot.gpus.len() as u32;
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n)).collect();
    let sub_rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    for (idx, gpu) in snapshot.gpus.iter().enumerate() {
        render_single_gpu(frame, sub_rects[idx], gpu, idx + 1);
    }
}

/// Render one GPU inside its own bordered sub-block. Title format is
/// `GPU (N)` where `N` is 1-indexed. Content lines:
///   1. GPU name (truncated to inner width)
///   2. `util ██░░░░░░░░ NN%`
///   3. `mem  ██░░░░░░░░ used/total  temp°C`
///
/// A sub-block that gets fewer than 3 content rows clips from the
/// bottom (util then mem drop off first).
fn render_single_gpu(frame: &mut Frame<'_>, area: Rect, gpu: &GpuStats, index: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(" GPU ({index}) "));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    // GPUs without telemetry (non-NVIDIA cards we can only enumerate
    // by name — iGPU, AMD/Intel dGPU) get a compact "no telemetry"
    // line instead of misleading `util 0%` / `mem n/a` bars. The
    // vendor hint helps the user understand why (only NVIDIA cards
    // report util/mem/temp via NVML today).
    let has_telemetry = gpu.utilization.is_some() || gpu.memory_total > 0;
    let name_line = Line::styled(
        truncate(&gpu.name, inner.width as usize),
        Style::default().fg(Color::White),
    );
    if !has_telemetry {
        let hint = if gpu.name.to_lowercase().contains("nvidia") {
            "no telemetry (NVML unavailable)"
        } else {
            "no telemetry (non-NVIDIA)"
        };
        let lines = vec![
            name_line,
            Line::styled(hint, Style::default().fg(Color::DarkGray)),
        ];
        Paragraph::new(lines).render(inner, frame.buffer_mut());
        return;
    }

    let util_pct = gpu.utilization.unwrap_or(0.0);
    let util_color = gpu
        .utilization
        .map(cpu_color_f32)
        .unwrap_or(Color::DarkGray);
    let util_bar = usage_bar((util_pct as f64) / 100.0, 10);
    let mem_ratio = if gpu.memory_total == 0 {
        0.0
    } else {
        gpu.memory_used as f64 / gpu.memory_total as f64
    };
    let mem_bar = usage_bar(mem_ratio, 10);
    let mem_label = if gpu.memory_total == 0 {
        "n/a".to_owned()
    } else {
        format!(
            "{}/{}",
            format_size(gpu.memory_used, DECIMAL),
            format_size(gpu.memory_total, DECIMAL)
        )
    };
    let temp = gpu
        .temperature
        .map(|t| format!("{t:.0}°C"))
        .unwrap_or_else(|| "n/a".to_owned());

    let lines = vec![
        name_line,
        Line::from(vec![
            Span::styled("util ", Style::default().fg(Color::DarkGray)),
            Span::styled(util_bar, Style::default().fg(util_color)),
            Span::raw(format!(" {util_pct:>3.0}%")),
        ]),
        Line::from(vec![
            Span::styled("mem  ", Style::default().fg(Color::DarkGray)),
            Span::styled(mem_bar, Style::default().fg(gauge_color(mem_ratio))),
            Span::raw(format!("  {mem_label}  {temp}")),
        ]),
    ];
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

fn render_docker_row(frame: &mut Frame<'_>, area: Rect, docker: &DockerStats) {
    let line = Line::from(vec![
        Span::styled(" Docker ", Style::default().fg(Color::Blue)),
        Span::styled(
            format!(" {} running", docker.running),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" · "),
        Span::styled(
            format!("{} paused", docker.paused),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" · "),
        Span::styled(
            format!("{} stopped", docker.stopped),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(format!("  (total {})", docker.total())),
    ]);
    Paragraph::new(line).render(area, frame.buffer_mut());
}

fn render_cgroup_row(frame: &mut Frame<'_>, area: Rect, cg: &crate::sysmon_model::CgroupInfo) {
    let line = Line::from(vec![
        Span::styled(" cgroup ", Style::default().fg(Color::Cyan)),
        Span::raw(truncate(&cg.path, area.width.saturating_sub(10) as usize)),
    ]);
    Paragraph::new(line).render(area, frame.buffer_mut());
}

fn render_top_procs_mini(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot) {
    // Fixed sort by CPU descending — this is a read-only preview of
    // the busiest processes so the user sees who's spending cycles
    // without leaving Overview. The main Processes view is where they
    // sort / filter / kill.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(
            " Top Processes ({}) ",
            snapshot.top_processes.len()
        ));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Reuse the process-table column formatter for visual parity with
    // the full Processes view. `top_processes` from the worker is
    // already sorted CPU-descending.
    let width = inner.width as usize;
    let name_width = width.saturating_sub(7 + 6 + 10 + 3);
    let dim = Style::default().fg(Color::DarkGray);
    let mut items: Vec<ListItem> = Vec::with_capacity(inner.height as usize);
    items.push(ListItem::new(Line::styled(
        format!(
            "{:>7} {:>6} {:>10} {:<name$}",
            "PID",
            "CPU%",
            "MEM",
            "NAME",
            name = name_width
        ),
        dim,
    )));
    let rows_to_show = (inner.height as usize).saturating_sub(1);
    for row in snapshot.top_processes.iter().take(rows_to_show) {
        let style = Style::default().fg(cpu_color_f32(row.cpu));
        let line = format!(
            "{:>7} {:>6.1} {:>10} {:<name$}",
            row.pid,
            row.cpu,
            format_size(row.memory, DECIMAL),
            truncate(&row.name, name_width),
            name = name_width,
        );
        items.push(ListItem::new(Line::styled(line, style)));
    }
    List::new(items).render(inner, frame.buffer_mut());
}

fn render_network(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot) {
    // Aggregate first, then the top-N interfaces by combined rate so
    // the busy links float to the top even when the system has 20+
    // virtual interfaces (docker0, veth*, tun*, ...).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(" Network ({} ifaces) ", snapshot.networks.len()));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let (rx_total, tx_total) = snapshot
        .networks
        .iter()
        .fold((0.0f64, 0.0f64), |(rx, tx), n| {
            (rx + n.rx_rate, tx + n.tx_rate)
        });
    let mut top: Vec<&crate::sysmon_model::NetworkStats> = snapshot.networks.iter().collect();
    top.sort_by(|a, b| {
        (b.rx_rate + b.tx_rate)
            .partial_cmp(&(a.rx_rate + a.tx_rate))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("total ", dim),
        Span::styled("↓ ", Style::default().fg(Color::Green)),
        Span::raw(format_rate(rx_total)),
        Span::raw("  "),
        Span::styled("↑ ", Style::default().fg(Color::Blue)),
        Span::raw(format_rate(tx_total)),
    ]));
    let name_col = inner.width.saturating_sub(28) as usize;
    let iface_rows = (inner.height as usize).saturating_sub(1);
    for iface in top.iter().take(iface_rows) {
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{:<width$} ",
                    truncate(&iface.name, name_col),
                    width = name_col
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled("↓ ", Style::default().fg(Color::Green)),
            Span::raw(format!("{:>10}", format_rate(iface.rx_rate))),
            Span::raw("  "),
            Span::styled("↑ ", Style::default().fg(Color::Blue)),
            Span::raw(format!("{:>10}", format_rate(iface.tx_rate))),
        ]));
    }
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

fn render_disk(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot) {
    // One line per mount: name + inline usage bar + used/total. Skip
    // mounts with zero total (pseudo filesystems on some hosts).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(" Disk ({} mounts) ", snapshot.disks.len()));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mounts: Vec<&crate::sysmon_model::DiskStats> =
        snapshot.disks.iter().filter(|d| d.total > 0).collect();
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    let capacity = (inner.height as usize).max(1);
    for disk in mounts.iter().take(capacity) {
        let used = disk.total.saturating_sub(disk.available);
        let ratio = used as f64 / disk.total as f64;
        let bar = usage_bar(ratio, 10);
        let mount = disk.mount.to_string_lossy();
        let mount_col = inner.width.saturating_sub(36).max(6) as usize;
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{:<width$} ",
                    truncate(&mount, mount_col),
                    width = mount_col
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled(bar, Style::default().fg(gauge_color(ratio))),
            Span::raw(format!(
                " {:>3.0}% {:>8} / {:<8}",
                ratio * 100.0,
                format_size(used, DECIMAL),
                format_size(disk.total, DECIMAL),
            )),
        ]));
    }
    if mounts.is_empty() {
        lines.push(Line::styled("no mounted filesystems detected", dim));
    }
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

fn render_system(frame: &mut Frame<'_>, area: Rect, snapshot: &Snapshot) {
    // Cross-platform info first — OS name, host, uptime, procs count
    // all work on Windows / macOS / Linux. Temp / Load are Unix-heavy
    // (Windows has neither a loadavg nor accessible thermal sensors
    // through sysinfo) so we only push those rows when the sampler
    // actually got a value back — otherwise the block would waste half
    // its rows on `n/a` on Windows.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" System ");
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);
    let width = inner.width as usize;
    let value_width = width.saturating_sub(7); // 6-char label + 1 space

    let mut lines: Vec<Line> = Vec::new();

    if let Some(os) = &snapshot.os_display {
        lines.push(Line::from(vec![
            Span::styled("OS    ", dim),
            Span::raw(truncate(os, value_width)),
        ]));
    }
    if let Some(host) = &snapshot.host_name {
        lines.push(Line::from(vec![
            Span::styled("Host  ", dim),
            Span::raw(truncate(host, value_width)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Up    ", dim),
        Span::raw(format_uptime(snapshot.uptime_seconds)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Procs ", dim),
        Span::raw(snapshot.top_processes.len().to_string()),
    ]));
    // Temp / Load come next — they're rare (Windows never has them
    // via sysinfo) but valuable when present. Freq falls to the bottom
    // so it drops off first when the block runs out of rows.
    if let Some(t) = snapshot.cpu_temp {
        lines.push(Line::from(vec![
            Span::styled("Temp  ", dim),
            Span::raw(format!("{t:.1}°C")),
        ]));
    }
    if let Some((a, b, c)) = snapshot.load_avg {
        lines.push(Line::from(vec![
            Span::styled("Load  ", dim),
            Span::raw(format!("{a:.2}  {b:.2}  {c:.2}")),
        ]));
    }
    if snapshot.cpu_frequency_mhz > 0 {
        lines.push(Line::from(vec![
            Span::styled("Freq  ", dim),
            Span::raw(format_frequency(snapshot.cpu_frequency_mhz)),
        ]));
    }

    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

/// Format seconds since boot as `Nd HH:MM` / `HH:MM:SS` / `Nm SSs`.
/// Compact enough to fit the narrow System column even on 30-col panes.
fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{mins:02}")
    } else if hours > 0 {
        format!("{hours:02}:{mins:02}:{s:02}")
    } else if mins > 0 {
        format!("{mins}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Format CPU frequency in MHz as `X.XX GHz` when >=1000 MHz, else
/// `X MHz` — mirrors what most system monitors show.
fn format_frequency(mhz: u64) -> String {
    if mhz >= 1000 {
        format!("{:.2} GHz", mhz as f64 / 1000.0)
    } else {
        format!("{mhz} MHz")
    }
}

/// `██████░░░░` style usage bar of `width` cells. Uses filled / empty
/// unicode blocks so the widget reads at a glance without a Gauge
/// widget's overhead (Gauge draws a full row; we need to inline it
/// with other columns).
fn usage_bar(ratio: f64, width: usize) -> String {
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        if i < filled {
            s.push('█');
        } else {
            s.push('░');
        }
    }
    s
}

// ─── Processes view ─────────────────────────────────────────────────

fn render_processes_view(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &Snapshot,
    cpu_history: &[f64],
    view: &ProcessView,
    cursor: usize,
) {
    // Small header keeps a CPU waveform visible while the user
    // scrolls through the table; everything else goes to the table.
    let header_rows = area.height.min(6).saturating_sub(1).clamp(3, 6);
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_rows), Constraint::Min(3)])
        .split(area);
    render_cpu_chart(frame, split[0], snapshot, cpu_history);
    render_processes_table(frame, split[1], view, cursor);
}

fn render_processes_table(frame: &mut Frame<'_>, area: Rect, view: &ProcessView, cursor: usize) {
    let header = format!(
        " Processes ({}) · sort: {:?} {} · filter: {} ",
        view.rows.len(),
        view.sort_key,
        match view.order {
            SortOrder::Ascending => "↑",
            SortOrder::Descending => "↓",
        },
        view.filter.as_deref().unwrap_or("—"),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(header);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Column widths tuned for narrow columns first: pid 7, cpu 6,
    // mem 10 (fits "12.34 GB"), name = remainder.
    let width = inner.width as usize;
    let name_width = width.saturating_sub(7 + 6 + 10 + 3);

    let mut items: Vec<ListItem> = Vec::with_capacity(view.rows.len() + 1);
    items.push(ListItem::new(Line::from(vec![Span::styled(
        format!(
            "{:>7} {:>6} {:>10} {:<name$}",
            "PID",
            "CPU%",
            "MEM",
            "NAME",
            name = name_width
        ),
        Style::default().fg(Color::DarkGray),
    )])));
    for (idx, row) in view.rows.iter().enumerate() {
        let base = Style::default();
        let style = if idx == cursor {
            base.bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let line = format!(
            "{:>7} {:>6.1} {:>10} {:<name$}",
            row.pid,
            row.cpu,
            format_size(row.memory, DECIMAL),
            truncate(&row.name, name_width),
            name = name_width,
        );
        items.push(ListItem::new(Line::styled(line, style)));
    }
    List::new(items).render(inner, frame.buffer_mut());
}

// ─── Shared bits ────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame<'_>, area: Rect, modal: &Modal, hint: Option<&str>) {
    let text = match modal {
        Modal::Filter { input } => format!("/ {input}_"),
        Modal::ConfirmKill { pid, name } => {
            format!("kill {name} (pid {pid})? [y/N]")
        }
        Modal::None => hint.unwrap_or("").to_string(),
    };
    let style = match modal {
        Modal::ConfirmKill { .. } => Style::default().fg(Color::Yellow),
        Modal::Filter { .. } => Style::default().fg(Color::Cyan),
        Modal::None => Style::default().fg(Color::DarkGray),
    };
    Paragraph::new(Line::styled(text, style)).render(area, frame.buffer_mut());
}

fn cpu_color(pct: f32) -> Color {
    cpu_color_f32(pct)
}

fn cpu_color_f32(pct: f32) -> Color {
    if pct < 30.0 {
        Color::Green
    } else if pct < 70.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn gauge_color(ratio: f64) -> Color {
    if ratio < 0.60 {
        Color::Green
    } else if ratio < 0.85 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        "0 B/s".to_owned()
    } else {
        format!("{}/s", format_size(bytes_per_sec.round() as u64, DECIMAL))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_owned()
    } else if max <= 1 {
        "…".to_owned()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn ctx() -> PaneRenderCtx<'static> {
        PaneRenderCtx {
            focused: true,
            title_override: None,
            focus_color: Color::Cyan,
        }
    }

    fn stub_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn tab_cycles_view_between_overview_and_processes() {
        let mut pane = SysmonPane::new();
        assert_eq!(pane.view, SysmonView::Overview);
        assert!(pane.on_key(stub_key(KeyCode::Tab)));
        assert_eq!(pane.view, SysmonView::Processes);
        assert!(pane.on_key(stub_key(KeyCode::Tab)));
        assert_eq!(pane.view, SysmonView::Overview);
    }

    #[test]
    fn sort_keys_switch_key_and_flip_on_repeat() {
        let mut pane = SysmonPane::new();
        assert!(pane.on_key(stub_key(KeyCode::Char('m'))));
        assert_eq!(pane.sort_key, SortKey::Memory);
        assert_eq!(pane.sort_order, SortOrder::Descending);
        assert!(pane.on_key(stub_key(KeyCode::Char('m'))));
        assert_eq!(pane.sort_order, SortOrder::Ascending);
        assert!(pane.on_key(stub_key(KeyCode::Char('c'))));
        assert_eq!(pane.sort_key, SortKey::Cpu);
        assert_eq!(pane.sort_order, SortOrder::Descending);
    }

    #[test]
    fn slash_enters_filter_modal_then_enter_applies() {
        let mut pane = SysmonPane::new();
        pane.on_key(stub_key(KeyCode::Char('/')));
        assert!(matches!(pane.modal, Modal::Filter { .. }));
        pane.on_key(stub_key(KeyCode::Char('c')));
        pane.on_key(stub_key(KeyCode::Char('o')));
        pane.on_key(stub_key(KeyCode::Char('d')));
        pane.on_key(stub_key(KeyCode::Char('e')));
        pane.on_key(stub_key(KeyCode::Enter));
        assert_eq!(pane.filter.as_deref(), Some("code"));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn confirm_kill_modal_ignores_stray_keys_and_dismisses_on_n() {
        let mut pane = SysmonPane::new();
        pane.modal = Modal::ConfirmKill {
            pid: 999_999,
            name: "ghost".into(),
        };
        assert!(!pane.on_key(stub_key(KeyCode::Char('z'))));
        assert!(matches!(pane.modal, Modal::ConfirmKill { .. }));
        assert!(pane.on_key(stub_key(KeyCode::Char('n'))));
        assert!(matches!(pane.modal, Modal::None));
    }

    #[test]
    fn format_uptime_covers_ranges() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(5), "5s");
        assert_eq!(format_uptime(65), "1m 05s");
        assert_eq!(format_uptime(3_600), "01:00:00");
        assert_eq!(format_uptime(3_665), "01:01:05");
        assert_eq!(format_uptime(90_061), "1d 01:01");
        assert_eq!(format_uptime(3 * 86_400 + 14 * 3_600 + 22 * 60), "3d 14:22");
    }

    #[test]
    fn format_frequency_switches_units_at_1ghz() {
        assert_eq!(format_frequency(0), "0 MHz");
        assert_eq!(format_frequency(800), "800 MHz");
        assert_eq!(format_frequency(999), "999 MHz");
        assert_eq!(format_frequency(1000), "1.00 GHz");
        assert_eq!(format_frequency(3400), "3.40 GHz");
    }

    #[test]
    fn render_system_windows_never_shows_temp_or_load_placeholder() {
        // Windows snapshot: no temp, no loadavg, but full cross-
        // platform metadata. The block must render OS / Host / Up /
        // Procs / Freq and MUST NOT show "n/a" for the missing rows.
        let mut pane = SysmonPane::new();
        pane.snapshot = Snapshot {
            generation: 1,
            cpu_per_core: vec![10.0, 20.0, 30.0, 40.0],
            cpu_avg: 25.0,
            cpu_frequency_mhz: 3200,
            cpu_temp: None,
            load_avg: None,
            host_name: Some("DESKTOP-XYZ".into()),
            os_display: Some("Windows 11 Pro".into()),
            uptime_seconds: 3 * 86_400 + 14 * 3_600 + 22 * 60,
            top_processes: (1..=3)
                .map(|i| crate::sysmon_model::ProcessInfo {
                    pid: i,
                    name: format!("p{i}"),
                    cpu: (i as f32) * 2.0,
                    memory: 10 * 1024,
                })
                .collect(),
            ..Snapshot::empty()
        };
        pane.applied_generation = 1;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(all.contains("Windows 11 Pro"), "OS row missing: {all}");
        assert!(all.contains("DESKTOP-XYZ"), "Host row missing");
        assert!(all.contains("3d 14:22"), "Uptime row missing");
        assert!(all.contains("3.20 GHz"), "Freq row missing");
        assert!(
            !all.contains("Temp  n/a"),
            "Temp row must not render `n/a` on Windows"
        );
        assert!(
            !all.contains("Load  n/a"),
            "Load row must not render `n/a` on Windows"
        );
    }

    #[test]
    fn render_system_linux_shows_temp_and_load_when_present() {
        let mut pane = SysmonPane::new();
        pane.snapshot = Snapshot {
            generation: 1,
            cpu_per_core: vec![50.0],
            cpu_avg: 50.0,
            cpu_frequency_mhz: 2400,
            cpu_temp: Some(58.5),
            load_avg: Some((1.20, 0.90, 0.70)),
            host_name: Some("box".into()),
            os_display: Some("Ubuntu 22.04".into()),
            uptime_seconds: 3_600,
            ..Snapshot::empty()
        };
        pane.applied_generation = 1;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(all.contains("58.5°C"), "Temp row missing: {all}");
        assert!(all.contains("1.20  0.90  0.70"), "Load row missing");
    }

    #[test]
    fn render_gpu_box_enumerates_multiple_gpus_with_indexed_titles() {
        // Two-GPU host: buffer must carry both `GPU (1)` and `GPU (2)`
        // titles stacked vertically, each with its own bordered block.
        let mut pane = SysmonPane::new();
        pane.snapshot = Snapshot {
            generation: 1,
            cpu_per_core: vec![10.0, 20.0],
            cpu_avg: 15.0,
            gpus: vec![
                GpuStats {
                    name: "GeForce RTX 3080".into(),
                    utilization: Some(42.0),
                    memory_used: 4 * 1024 * 1024 * 1024,
                    memory_total: 10 * 1024 * 1024 * 1024,
                    temperature: Some(58.0),
                },
                GpuStats {
                    name: "GeForce RTX 4090".into(),
                    utilization: Some(75.0),
                    memory_used: 12 * 1024 * 1024 * 1024,
                    memory_total: 24 * 1024 * 1024 * 1024,
                    temperature: Some(65.0),
                },
            ],
            ..Snapshot::empty()
        };
        pane.applied_generation = 1;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(all.contains("GPU (1)"), "first GPU title missing: {all}");
        assert!(all.contains("GPU (2)"), "second GPU title missing");
        assert!(all.contains("GeForce RTX 3080"), "first GPU name missing");
        assert!(all.contains("GeForce RTX 4090"), "second GPU name missing");

        // Titles must appear on DIFFERENT rows (stacked, not side-by-side).
        let row_of = |needle: &str| -> Option<u16> {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                if row.contains(needle) {
                    return Some(y);
                }
            }
            None
        };
        let row_1 = row_of("GPU (1)").expect("row for GPU (1)");
        let row_2 = row_of("GPU (2)").expect("row for GPU (2)");
        assert!(
            row_1 < row_2,
            "GPU (2) must sit BELOW GPU (1) — got rows {row_1} vs {row_2}"
        );
    }

    #[test]
    fn render_gpu_box_empty_shows_placeholder_no_index() {
        // No GPU → single "GPU" block (no "(N)" index) with a
        // "no NVIDIA GPU detected" placeholder line.
        let mut pane = SysmonPane::new();
        pane.snapshot = Snapshot {
            generation: 1,
            cpu_per_core: vec![10.0],
            cpu_avg: 10.0,
            ..Snapshot::empty()
        };
        pane.applied_generation = 1;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(all.contains(" GPU "), "single GPU block title missing");
        assert!(
            !all.contains("GPU (1)"),
            "empty state must NOT show an indexed title: {all}"
        );
        assert!(
            all.contains("no NVIDIA GPU detected"),
            "placeholder line missing"
        );
    }

    #[test]
    fn render_fits_inside_area_and_does_not_panic() {
        let mut pane = SysmonPane::new();
        // Seed a synthetic snapshot so the pane has content to draw
        // without waiting on the worker.
        pane.snapshot = Snapshot {
            generation: 1,
            cpu_per_core: vec![10.0, 25.0, 60.0, 90.0],
            cpu_avg: 46.25,
            memory: crate::sysmon_model::MemoryStats {
                used: 8 * 1024 * 1024 * 1024,
                total: 16 * 1024 * 1024 * 1024,
            },
            swap: crate::sysmon_model::MemoryStats {
                used: 0,
                total: 4 * 1024 * 1024 * 1024,
            },
            top_processes: (1..=5)
                .map(|i| crate::sysmon_model::ProcessInfo {
                    pid: i,
                    name: format!("proc-{i}"),
                    cpu: (i as f32) * 4.0,
                    memory: (i as u64) * 128 * 1024 * 1024,
                })
                .collect(),
            gpus: vec![crate::sysmon_model::GpuStats {
                name: "GeForce RTX 3080".into(),
                utilization: Some(42.0),
                memory_used: 4 * 1024 * 1024 * 1024,
                memory_total: 10 * 1024 * 1024 * 1024,
                temperature: Some(58.0),
            }],
            docker: Some(DockerStats {
                running: 3,
                paused: 0,
                stopped: 2,
            }),
            ..Snapshot::empty()
        };
        pane.applied_generation = 1;
        // Prime some waveform history so the chart draws non-trivially.
        pane.cpu_history = (0..120).map(|i| (i % 90) as f64).collect();

        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .expect("overview render must not panic");

        pane.view = SysmonView::Processes;
        term.draw(|f| {
            pane.render(f.area(), f, &ctx());
        })
        .expect("processes view must not panic");
    }
}
