//! Native world-map + timezones pane (6th tab in the left-bottom group).
//!
//! Renders a braille equirectangular globe (2:1 landscape) with a day/night
//! terminator anchored on "now", plus a highlighted `◉` home marker at the
//! user's IANA-resolved coordinates and a marker per configured zone.
//!
//! Layout tiers by inner-rect size (§7.1 of `docs/rimeterm-worldmap-design.md`):
//!
//! | Inner              | Layout                                              |
//! | ------------------ | --------------------------------------------------- |
//! | `< 40 cols`        | Status line only                                    |
//! | `40..=59 / <12 rows`| Map fills rect; no legend, no side list             |
//! | `60..=99 & ≥12 rows`| Legend (1) + map + status (1)                       |
//! | `≥ 100 & ≥15 rows` | Legend (1) + [map · side list] + status (1)         |
//!
//! Key bindings:
//!
//! | Key           | Action                                                |
//! | ------------- | ----------------------------------------------------- |
//! | `j / k / ↓ ↑` | Move cursor in the side zone list                     |
//! | `a`           | Open add-zone modal (fuzzy over `ZONE_COORDS`)        |
//! | `x`           | Delete highlighted row (with `y/n` confirm)           |
//! | `h`           | Snap "home" cursor back to the auto-resolved zone     |
//! | `r`           | Force an immediate repaint                            |
//! | `Enter` / `Esc` | Confirm / cancel active modal                        |
//!
//! Attribution: the map subsystem (braille canvas, equirectangular projection,
//! NOAA subsolar geometry, coastline + IANA coordinate tables, ZoneHandle
//! parsing) is ported from MIT-licensed
//! [`zonetimeline-tui`](https://github.com/findyourexit/zonetimeline-tui)
//! @ v0.4.0 and lives in the `rimeterm-zones` crate.

use std::any::Any;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use rimeterm_config::ZonesConfig;
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_zones::{
    BrailleCanvas, COASTLINE, Placement, SunPosition, ZONE_COORDS, ZoneEntry, ZoneHandle, ZoneList,
    is_night, lat_to_norm, locate, lon_to_norm, norm_to_lat, norm_to_lon, parse_zone, subsolar,
};

/// Minimum inner width to render a legend row.
const MIN_LEGEND_COLS: u16 = 60;
/// Minimum inner width to unlock the side-list layout.
const MIN_SIDE_LIST_COLS: u16 = 100;
/// Minimum inner height for legend + map + status.
const MIN_STANDARD_ROWS: u16 = 12;
/// Minimum inner height for the side-list layout.
const MIN_SIDE_LIST_ROWS: u16 = 15;
/// Fixed width of the side zone list panel (chars).
const SIDE_LIST_WIDTH: u16 = 26;
/// Refresh cadence when the pane is hidden — no repaints, just a snap on
/// `set_visible(true)` (§8 of the design doc).
const HIDDEN_REFRESH: Duration = Duration::from_secs(60 * 10);
/// Max add-zone modal search results.
const ADD_MODAL_LIMIT: usize = 12;

/// Modal state — nothing, add-zone search, or delete confirm.
#[derive(Clone, Debug, Default)]
enum Modal {
    #[default]
    None,
    Add {
        input: String,
        cursor: usize,
    },
    ConfirmDelete {
        index: usize,
    },
}

/// Native ZonesPane provider.
pub struct ZonesPane {
    id: PaneId,
    config: ZonesConfig,
    list: ZoneList,
    watchlist_path: PathBuf,
    /// Resolved handle for the auto-detected home zone; `None` when the OS
    /// gave us nothing useful. Config override takes precedence.
    home: Option<ZoneHandle>,
    /// Cursor into `list.entries`.
    selected: usize,
    modal: Modal,
    /// When the next scheduled repaint fires (drives `poll_background`).
    next_tick: Instant,
    visible: bool,
    hint: Option<String>,
}

impl ZonesPane {
    pub fn new(config: ZonesConfig, watchlist_path: PathBuf) -> Self {
        let list = match ZoneList::load_or_seed(&watchlist_path) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %watchlist_path.display(),
                    "failed to load zones list; seeding defaults"
                );
                ZoneList::seeded()
            }
        };
        let home = resolve_home(&config);
        Self {
            id: PaneId::next(),
            config,
            list,
            watchlist_path,
            home,
            selected: 0,
            modal: Modal::None,
            next_tick: Instant::now(),
            visible: false,
            hint: None,
        }
    }

    fn save_list(&self) {
        if let Err(e) = self.list.save(&self.watchlist_path) {
            tracing::warn!(
                error = %e,
                path = %self.watchlist_path.display(),
                "failed to persist zones list"
            );
        }
    }

    fn snap_tick(&mut self) {
        self.next_tick = Instant::now();
    }

    fn refresh_interval(&self) -> Duration {
        if self.visible {
            Duration::from_secs(self.config.refresh_secs.max(1) as u64)
        } else {
            HIDDEN_REFRESH
        }
    }

    fn clamp_selected(&mut self) {
        let n = self.list.entries.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Recompute the home cursor from the config + OS. Called on `h`.
    fn snap_home(&mut self) {
        self.home = resolve_home(&self.config);
        self.hint = Some(match &self.home {
            Some(handle) => format!("home → {}", zone_label(handle)),
            None => "home unresolved (OS returned nothing)".to_string(),
        });
    }

    fn open_add(&mut self) {
        self.modal = Modal::Add {
            input: String::new(),
            cursor: 0,
        };
    }

    fn confirm_delete(&mut self) {
        if self.selected < self.list.entries.len() {
            self.modal = Modal::ConfirmDelete {
                index: self.selected,
            };
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let n = self.list.entries.len();
        if n == 0 {
            return;
        }
        let mut sel = self.selected as i32 + delta;
        if sel < 0 {
            sel = 0;
        }
        if sel >= n as i32 {
            sel = n as i32 - 1;
        }
        self.selected = sel as usize;
    }

    fn on_add_key(&mut self, key: KeyEvent) -> bool {
        // Snapshot inputs so we can mutate `self.list` inside the same match.
        let Modal::Add { input, cursor } = &mut self.modal else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {
                self.modal = Modal::None;
            }
            KeyCode::Enter => {
                let results = search_zones(input);
                if let Some(&name) = results.get(*cursor) {
                    let added = self.list.push_unique(name);
                    if added {
                        self.selected = self.list.entries.len().saturating_sub(1);
                        self.save_list();
                        self.hint = Some(format!("added {name}"));
                    } else {
                        self.hint = Some(format!("{name} already in list"));
                    }
                }
                self.modal = Modal::None;
                self.snap_tick();
            }
            KeyCode::Backspace => {
                input.pop();
                *cursor = 0;
            }
            KeyCode::Up => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
            KeyCode::Down => {
                let results_len = search_zones(input).len().min(ADD_MODAL_LIMIT);
                if *cursor + 1 < results_len {
                    *cursor += 1;
                }
            }
            KeyCode::Char(c) => {
                input.push(c);
                *cursor = 0;
            }
            _ => {}
        }
        true
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> bool {
        let Modal::ConfirmDelete { index } = self.modal else {
            return false;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(entry) = self.list.remove(index) {
                    self.hint = Some(format!("removed {}", entry.input));
                    self.save_list();
                    self.clamp_selected();
                }
                self.modal = Modal::None;
                self.snap_tick();
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.modal = Modal::None;
            }
            _ => {}
        }
        true
    }
}

impl PaneProvider for ZonesPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        "zones"
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
        render_root(self, area, frame, border);
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
        match &self.modal {
            Modal::Add { .. } => return self.on_add_key(key),
            Modal::ConfirmDelete { .. } => return self.on_confirm_key(key),
            Modal::None => {}
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_cursor(1);
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_cursor(-1);
                true
            }
            KeyCode::Char('a') => {
                self.open_add();
                true
            }
            KeyCode::Char('x') => {
                self.confirm_delete();
                true
            }
            KeyCode::Char('h') => {
                self.snap_home();
                self.snap_tick();
                true
            }
            KeyCode::Char('r') => {
                self.snap_tick();
                true
            }
            _ => false,
        }
    }

    fn poll_background(&mut self) -> bool {
        if !self.visible {
            return false;
        }
        if Instant::now() >= self.next_tick {
            self.next_tick = Instant::now() + self.refresh_interval();
            true
        } else {
            false
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if visible != self.visible {
            self.visible = visible;
            if visible {
                self.snap_tick();
            }
        }
    }

    fn reload(&mut self) {
        if let Ok(list) = ZoneList::load_or_seed(&self.watchlist_path) {
            self.list = list;
            self.clamp_selected();
            self.hint = Some("zones list reloaded".to_string());
        }
    }
}

/// Resolve the home zone: config override wins, else `parse_zone("local")`.
/// A parse failure returns `None` and the pane just doesn't paint a home marker.
fn resolve_home(config: &ZonesConfig) -> Option<ZoneHandle> {
    if let Some(home) = &config.home {
        match parse_zone(home) {
            Ok(handle) => return Some(handle),
            Err(e) => {
                tracing::warn!(zone = %home, error = %e, "config zones.home unresolved");
            }
        }
    }
    parse_zone("local").ok()
}

/// Human-friendly label for a handle.
fn zone_label(handle: &ZoneHandle) -> String {
    match handle {
        ZoneHandle::Named(tz) => tz.name().to_string(),
        ZoneHandle::Fixed(offset) => {
            rimeterm_zones::format_utc_offset_short(offset.local_minus_utc())
        }
    }
}

/// Row availability class — same trichotomy zonetimeline uses, but with the
/// default work window baked into config (no per-zone override in v0).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Availability {
    Core,
    Shoulder,
    Off,
}

impl Availability {
    fn color(self) -> Color {
        match self {
            Self::Core => Color::Green,
            Self::Shoulder => Color::Yellow,
            Self::Off => Color::DarkGray,
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Shoulder => "shoulder",
            Self::Off => "off",
        }
    }
}

/// Classify a local minute-of-day against the default `HH:MM-HH:MM` window.
fn classify(minute: u16, window: &str, shoulder_hours: u16) -> Availability {
    let Some((start, end)) = parse_window(window) else {
        return Availability::Off;
    };
    let shoulder = shoulder_hours.saturating_mul(60);
    if minute >= start && minute < end {
        Availability::Core
    } else if minute + shoulder >= start && minute < start
        || minute >= end && minute < end.saturating_add(shoulder)
    {
        Availability::Shoulder
    } else {
        Availability::Off
    }
}

fn parse_window(spec: &str) -> Option<(u16, u16)> {
    let (a, b) = spec.split_once('-')?;
    Some((parse_hhmm(a)?, parse_hhmm(b)?))
}

fn parse_hhmm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let h: u16 = h.parse().ok()?;
    let m: u16 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Marker for one row on the map (home or a `ZoneList` entry).
#[derive(Clone, Debug)]
struct Marker {
    lat: f64,
    lon: f64,
    placement: Placement,
    color: Color,
    night: bool,
    time: String,
    is_home: bool,
    selected: bool,
}

fn collect_markers(pane: &ZonesPane, cursor: DateTime<Utc>, sun: &SunPosition) -> Vec<Marker> {
    let mut out = Vec::with_capacity(pane.list.entries.len());
    // Dedupe by resolved handle. `local` and, say, an explicit `Asia/Shanghai`
    // collapse to the same `Named(Tz)` on a Shanghai-local box; without this
    // filter the map would paint the SAME cell twice, and the label pass
    // would place its `HH:MM` badge to the RIGHT of the marker on the first
    // draw and to the LEFT of the marker on the second — the "two times
    // flanking the home ◉" bug. First entry wins, so its label lands on the
    // right (label pass prefers right-of-marker); every later duplicate is
    // dropped from the marker set. Side list still iterates
    // `pane.list.entries` directly and is unaffected.
    let mut seen: Vec<ZoneHandle> = Vec::new();

    // Home is drawn AS one of the list entries — the row whose zone matches
    // `pane.home` gets promoted to the ◉ purple home marker instead of
    // rendering a second glyph on top of the same coordinate. If no entry
    // matches (e.g. user deleted `local`), no home marker appears.
    for (idx, entry) in pane.list.entries.iter().enumerate() {
        let Ok(handle) = parse_zone(&entry.input) else {
            continue;
        };
        if seen.contains(&handle) {
            continue;
        }
        seen.push(handle);
        let loc = locate(&handle, cursor);
        let is_home = pane.home.as_ref() == Some(&handle);
        let (color, night) = if is_home {
            // Purple pin, ignore work-hour availability tint. `is_night` is
            // still computed from the zone's own coord so an off-hours home
            // still reads as "night" on the terminator.
            (Color::LightMagenta, is_night(sun, loc.lat, loc.lon))
        } else {
            let minute = handle.minute_of_day(cursor);
            let avail = classify(
                minute,
                &pane.config.default_window,
                pane.config.shoulder_hours,
            );
            (avail.color(), is_night(sun, loc.lat, loc.lon))
        };
        out.push(Marker {
            lat: loc.lat,
            lon: loc.lon,
            placement: loc.placement,
            color,
            night,
            time: handle.local_time(cursor).format("%H:%M").to_string(),
            is_home,
            selected: idx == pane.selected,
        });
    }
    out
}

fn render_root(pane: &mut ZonesPane, area: Rect, frame: &mut Frame<'_>, border: Style) {
    let cursor = Utc::now();
    let title = format!(" zones · {} UTC ", cursor.format("%Y-%m-%d %H:%M"));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Tier 1: too small for anything but a single status line.
    if inner.width < 40 {
        render_tiny(frame, inner, pane, cursor);
        return;
    }

    let show_legend = inner.width >= MIN_LEGEND_COLS && inner.height >= MIN_STANDARD_ROWS;
    let show_side_list = pane.config.show_side_list
        && inner.width >= MIN_SIDE_LIST_COLS
        && inner.height >= MIN_SIDE_LIST_ROWS;

    let mut constraints = Vec::with_capacity(3);
    if show_legend {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut ri = 0;
    if show_legend {
        render_legend(frame, rows[ri]);
        ri += 1;
    }
    let middle = rows[ri];
    ri += 1;

    if show_side_list {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(SIDE_LIST_WIDTH)])
            .split(middle);
        render_map(frame, cols[0], pane, cursor);
        render_side_list(frame, cols[1], pane, cursor);
    } else {
        render_map(frame, middle, pane, cursor);
    }

    render_status(frame, rows[ri], pane, cursor);
    render_modal(frame, inner, pane);
}

fn render_tiny(frame: &mut Frame<'_>, area: Rect, pane: &ZonesPane, cursor: DateTime<Utc>) {
    let text = match &pane.home {
        Some(h) => format!(
            "{} · {}",
            zone_label(h),
            h.local_time(cursor).format("%H:%M")
        ),
        None => cursor.format("%H:%M UTC").to_string(),
    };
    Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::Gray),
    )))
    .render(area, frame.buffer_mut());
}

fn render_legend(frame: &mut Frame<'_>, area: Rect) {
    let muted = Style::default().fg(Color::DarkGray);
    let bold_fg = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::styled("● ", bold_fg(Color::Green)),
        Span::styled("Core  ", muted),
        Span::styled("● ", bold_fg(Color::Yellow)),
        Span::styled("Shoulder  ", muted),
        Span::styled("● ", bold_fg(Color::DarkGray)),
        Span::styled("Off  ", muted),
        Span::styled("○ ", bold_fg(Color::DarkGray)),
        Span::styled("Offset  ", muted),
        Span::styled("◉ ", bold_fg(Color::LightMagenta)),
        Span::styled("Home", muted),
    ];
    Paragraph::new(Line::from(truncate_spans(spans, area.width as usize)))
        .render(area, frame.buffer_mut());
}

fn truncate_spans(spans: Vec<Span<'static>>, budget: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= budget {
        return spans;
    }
    if budget < 3 {
        return vec![Span::styled("...", Style::default().fg(Color::DarkGray))];
    }
    let mut remaining = budget - 3;
    let mut out = Vec::with_capacity(spans.len() + 1);
    for span in spans {
        let n = span.content.chars().count();
        if n <= remaining {
            remaining -= n;
            out.push(span);
        } else {
            if remaining > 0 {
                let s: String = span.content.chars().take(remaining).collect();
                out.push(Span::styled(s, span.style));
            }
            break;
        }
    }
    out.push(Span::styled("...", Style::default().fg(Color::DarkGray)));
    out
}

/// Cell dimensions of the largest world that fits at the natural
/// equirectangular aspect ratio (2:1 geographic, rendered as 2:1
/// **visual** on typical 1:2-aspect terminal cells). Braille packs
/// 2×4 dots per cell, so a target of `cols == 4 * rows` cells makes
/// the dot grid `8*rows × 4*rows` = 2:1, matching the projection.
/// The remainder is letterboxed.
fn map_dimensions(cols: u16, rows: u16) -> (u16, u16) {
    let target = rows.saturating_mul(4);
    if cols >= target {
        (target, rows)
    } else {
        let r = cols / 4;
        (r.saturating_mul(4), r)
    }
}

fn coastline_canvas(cols: u16, rows: u16) -> Rc<BrailleCanvas> {
    thread_local! {
        static CACHE: RefCell<Option<(u16, u16, Rc<BrailleCanvas>)>> = const { RefCell::new(None) };
    }
    CACHE.with_borrow_mut(|cache| {
        if let Some((w, h, canvas)) = cache
            && *w == cols
            && *h == rows
        {
            return Rc::clone(canvas);
        }
        let mut canvas = BrailleCanvas::new(cols, rows);
        for line in COASTLINE {
            for pair in line.windows(2) {
                canvas.stroke_geo(
                    pair[0].0 as f64,
                    pair[0].1 as f64,
                    pair[1].0 as f64,
                    pair[1].1 as f64,
                );
            }
        }
        let canvas = Rc::new(canvas);
        *cache = Some((cols, rows, Rc::clone(&canvas)));
        canvas
    })
}

fn render_map(frame: &mut Frame<'_>, area: Rect, pane: &ZonesPane, cursor: DateTime<Utc>) {
    let (cols, rows) = (area.width, area.height);
    if cols == 0 || rows == 0 {
        return;
    }
    let (unit_w, unit_h) = map_dimensions(cols, rows);
    if unit_w == 0 || unit_h == 0 {
        return;
    }
    let ox = ((cols - unit_w) / 2) as i32;
    let oy = (rows - unit_h) / 2;

    let sun = subsolar(cursor);
    let canvas = coastline_canvas(unit_w, unit_h);
    let buf = frame.buffer_mut();

    // Paint the void surrounding the letterboxed globe.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(Color::Rgb(0x05, 0x08, 0x12)));
            }
        }
    }

    // Coastline + day/night shading.
    let (x0, x1) = (ox as u16, ox as u16 + unit_w);
    for cy in 0..unit_h {
        for cx in x0..x1 {
            let u = (cx as i32 - ox) as u16;
            let lon = norm_to_lon((u as f64 + 0.5) / unit_w as f64);
            let lat = norm_to_lat((cy as f64 + 0.5) / unit_h as f64);
            let night = is_night(&sun, lat, lon);
            let bg = if night {
                Color::Rgb(0x07, 0x0a, 0x16)
            } else {
                Color::Rgb(0x14, 0x1d, 0x33)
            };
            let (glyph, fg) = match canvas.glyph(u, cy) {
                Some(g) => (
                    g,
                    if night {
                        Color::Rgb(0x4a, 0x64, 0x88)
                    } else {
                        Color::Rgb(0x7e, 0xa9, 0xd6)
                    },
                ),
                None => (' ', Color::White),
            };
            if let Some(cell) = buf.cell_mut((area.x + cx, area.y + oy + cy)) {
                cell.set_char(glyph);
                cell.set_style(Style::default().fg(fg).bg(bg));
            }
        }
    }

    // Markers + inline HH:MM labels (declutter against occupied grid).
    let markers = collect_markers(pane, cursor, &sun);
    let mut occupied = vec![false; cols as usize * unit_h as usize];
    // Draw non-home first, then home on top so the ◉ wins any collision.
    for pass in [false, true] {
        for m in markers.iter().filter(|m| m.is_home == pass) {
            let (mx, my) = marker_cell(m.lon, m.lat, unit_w, unit_h);
            let ax = ox as u16 + mx;
            if ax >= cols || my >= unit_h {
                continue;
            }
            occupied[my as usize * cols as usize + ax as usize] = true;
            draw_marker_cell(buf, area.x + ax, area.y + oy + my, m);
        }
    }
    for m in &markers {
        let (mx, my) = marker_cell(m.lon, m.lat, unit_w, unit_h);
        let anchor_x = ox as u16 + mx;
        if my >= unit_h {
            continue;
        }
        place_label(buf, area, cols, oy, anchor_x, my, m, unit_h, &mut occupied);
    }
}

fn marker_cell(lon: f64, lat: f64, cols: u16, rows: u16) -> (u16, u16) {
    let cx = (lon_to_norm(lon) * cols as f64).floor() as i64;
    let cy = (lat_to_norm(lat) * rows as f64).floor() as i64;
    (
        cx.clamp(0, cols as i64 - 1) as u16,
        cy.clamp(0, rows as i64 - 1) as u16,
    )
}

fn draw_marker_cell(buf: &mut ratatui::buffer::Buffer, bx: u16, by: u16, m: &Marker) {
    let (glyph, fg) = if m.is_home {
        ('◉', Color::LightMagenta)
    } else if m.selected {
        ('◉', Color::White)
    } else {
        (
            match m.placement {
                Placement::Geographic => '●',
                Placement::Offset => '○',
            },
            m.color,
        )
    };
    let bg = if m.night {
        Color::Rgb(0x07, 0x0a, 0x16)
    } else {
        Color::Rgb(0x14, 0x1d, 0x33)
    };
    if let Some(cell) = buf.cell_mut((bx, by)) {
        cell.set_char(glyph);
        cell.set_style(Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD));
    }
}

#[allow(clippy::too_many_arguments)]
fn place_label(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    cols: u16,
    oy: u16,
    mx: u16,
    my: u16,
    m: &Marker,
    unit_h: u16,
    occupied: &mut [bool],
) {
    let text: Vec<char> = m.time.chars().collect();
    let len = text.len() as u16;
    if len == 0 || len >= cols {
        return;
    }
    let row = my as usize * cols as usize;
    let free = |start: u16, occ: &[bool]| -> bool {
        start + len <= cols && (0..len).all(|i| !occ[row + (start + i) as usize])
    };
    let right = mx.saturating_add(1);
    let left = mx.saturating_sub(len);
    let start = if free(right, occupied) {
        Some(right)
    } else if mx >= len && free(left, occupied) {
        Some(left)
    } else if m.is_home || m.selected {
        Some(right.min(cols.saturating_sub(len)))
    } else {
        None
    };
    let Some(start) = start else {
        return;
    };
    let bg = if m.night {
        Color::Rgb(0x07, 0x0a, 0x16)
    } else {
        Color::Rgb(0x14, 0x1d, 0x33)
    };
    let fg = if m.is_home {
        Color::LightMagenta
    } else if m.selected {
        Color::White
    } else {
        Color::Rgb(0xd7, 0xdd, 0xea)
    };
    for (i, ch) in text.iter().enumerate() {
        let x = start + i as u16;
        if x >= cols {
            break;
        }
        occupied[row + x as usize] = true;
        if my >= unit_h {
            continue;
        }
        if let Some(cell) = buf.cell_mut((area.x + x, area.y + oy + my)) {
            cell.set_char(*ch);
            cell.set_style(Style::default().fg(fg).bg(bg));
        }
    }
}

fn render_side_list(frame: &mut Frame<'_>, area: Rect, pane: &ZonesPane, cursor: DateTime<Utc>) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(pane.list.entries.len());
    // Sort visually by longitude (west→east) so the list mirrors the map,
    // but keep the model order for delete/edit; we sort a copy of indices.
    let mut order: Vec<usize> = (0..pane.list.entries.len()).collect();
    order.sort_by(|&a, &b| {
        let la = zone_lon(&pane.list.entries[a], cursor);
        let lb = zone_lon(&pane.list.entries[b], cursor);
        la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
    });
    for idx in order {
        let entry = &pane.list.entries[idx];
        let (glyph, color, time, label) = match parse_zone(&entry.input) {
            Ok(handle) => {
                let minute = handle.minute_of_day(cursor);
                let avail = classify(
                    minute,
                    &pane.config.default_window,
                    pane.config.shoulder_hours,
                );
                let loc = locate(&handle, cursor);
                let glyph = match loc.placement {
                    Placement::Geographic => '●',
                    Placement::Offset => '○',
                };
                let time = handle.local_time(cursor).format("%H:%M").to_string();
                let label = entry
                    .label
                    .clone()
                    .unwrap_or_else(|| ZoneHandle::display_label(&entry.input));
                (glyph, avail.color(), time, label)
            }
            Err(_) => ('✗', Color::Red, "--:--".to_string(), entry.input.clone()),
        };
        let selected = idx == pane.selected;
        let base = if selected {
            Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(0x1e, 0x1e, 0x2e))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let line = Line::from(vec![
            Span::styled(
                format!("{glyph} "),
                base.fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{time}  "), base),
            Span::styled(truncate(&label, inner.width as usize - 8), base),
        ]);
        rows.push(line);
    }
    Paragraph::new(rows).render(inner, frame.buffer_mut());
}

fn zone_lon(entry: &ZoneEntry, cursor: DateTime<Utc>) -> f64 {
    parse_zone(&entry.input)
        .map(|h| locate(&h, cursor).lon)
        .unwrap_or(0.0)
}

fn truncate(s: &str, budget: usize) -> String {
    if s.chars().count() <= budget {
        s.to_string()
    } else if budget <= 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(budget - 1).collect();
        out.push('…');
        out
    }
}

fn render_status(frame: &mut Frame<'_>, area: Rect, pane: &ZonesPane, cursor: DateTime<Utc>) {
    // Compose the "▸ zone · HH:MM badge  ☀ over Nº" line.
    let sun = subsolar(cursor);
    let (label, time, badge, badge_color) = selected_detail(pane, cursor);
    let (deg, hemi) = if sun.lon >= 0.0 {
        (sun.lon, 'E')
    } else {
        (-sun.lon, 'W')
    };
    let mut spans = vec![
        Span::styled(
            "▸ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{label}  "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{time}  "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(badge, Style::default().fg(badge_color)),
        Span::styled(
            format!("    ☀ over {deg:.0}°{hemi}"),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some(hint) = &pane.hint {
        spans.push(Span::styled(
            format!("    {hint}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    Paragraph::new(Line::from(truncate_spans(spans, area.width as usize)))
        .render(area, frame.buffer_mut());
}

fn selected_detail(pane: &ZonesPane, cursor: DateTime<Utc>) -> (String, String, String, Color) {
    if let Some(entry) = pane.list.entries.get(pane.selected)
        && let Ok(handle) = parse_zone(&entry.input)
    {
        let label = entry
            .label
            .clone()
            .unwrap_or_else(|| ZoneHandle::display_label(&entry.input));
        let time = handle.local_time(cursor).format("%H:%M").to_string();
        let minute = handle.minute_of_day(cursor);
        let avail = classify(
            minute,
            &pane.config.default_window,
            pane.config.shoulder_hours,
        );
        return (label, time, avail.badge().to_string(), avail.color());
    }
    // No selection or the row failed to parse: fall back to home.
    if let Some(handle) = &pane.home {
        return (
            format!("{} (home)", zone_label(handle)),
            handle.local_time(cursor).format("%H:%M").to_string(),
            "home".to_string(),
            Color::LightMagenta,
        );
    }
    (
        "UTC".to_string(),
        cursor.format("%H:%M").to_string(),
        "reference".to_string(),
        Color::DarkGray,
    )
}

fn search_zones(query: &str) -> Vec<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        // Empty query: show the top of the alphabet so users see something.
        return ZONE_COORDS
            .iter()
            .take(ADD_MODAL_LIMIT)
            .map(|(name, _, _)| *name)
            .collect();
    }
    let mut starts: Vec<&'static str> = Vec::new();
    let mut contains: Vec<&'static str> = Vec::new();
    for (name, _, _) in ZONE_COORDS {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with(&q) {
            starts.push(*name);
        } else if lower.contains(&q) {
            contains.push(*name);
        }
        if starts.len() >= ADD_MODAL_LIMIT {
            break;
        }
    }
    let mut out = starts;
    if out.len() < ADD_MODAL_LIMIT {
        for c in contains {
            out.push(c);
            if out.len() >= ADD_MODAL_LIMIT {
                break;
            }
        }
    }
    out
}

fn render_modal(frame: &mut Frame<'_>, inner: Rect, pane: &ZonesPane) {
    match &pane.modal {
        Modal::None => {}
        Modal::Add { input, cursor } => render_add_modal(frame, inner, input, *cursor),
        Modal::ConfirmDelete { index } => {
            if let Some(entry) = pane.list.entries.get(*index) {
                render_confirm_modal(frame, inner, &entry.input);
            }
        }
    }
}

fn render_add_modal(frame: &mut Frame<'_>, inner: Rect, input: &str, cursor: usize) {
    let results = search_zones(input);
    let height = (results.len().min(ADD_MODAL_LIMIT) as u16 + 4).min(inner.height);
    let width = inner.width.min(60).max(30);
    let x = inner.x + (inner.width.saturating_sub(width)) / 2;
    let y = inner.y + (inner.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    // Paint an opaque backdrop so the map doesn't bleed through.
    let buf = frame.buffer_mut();
    for yy in area.y..area.y + area.height {
        for xx in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((xx, yy)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x2e)));
            }
        }
    }

    let block = Block::default()
        .title(" add zone ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let modal_inner = block.inner(area);
    block.render(area, buf);

    if modal_inner.height == 0 {
        return;
    }

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(modal_inner);

    let prompt = Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(input.to_string(), Style::default().fg(Color::White)),
        Span::styled(
            "_",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);
    Paragraph::new(prompt).render(split[0], frame.buffer_mut());

    let rows: Vec<Line<'static>> = results
        .iter()
        .take(split[1].height as usize)
        .enumerate()
        .map(|(idx, name)| {
            let selected = idx == cursor;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(format!(" {name} "), style))
        })
        .collect();
    Paragraph::new(rows).render(split[1], frame.buffer_mut());
}

fn render_confirm_modal(frame: &mut Frame<'_>, inner: Rect, entry: &str) {
    let width = inner.width.min(50).max(30);
    let height = 5u16.min(inner.height);
    let x = inner.x + (inner.width.saturating_sub(width)) / 2;
    let y = inner.y + (inner.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    let buf = frame.buffer_mut();
    for yy in area.y..area.y + area.height {
        for xx in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((xx, yy)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x2e)));
            }
        }
    }
    let block = Block::default()
        .title(" remove zone ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let modal_inner = block.inner(area);
    block.render(area, buf);
    let lines = vec![
        Line::from(Span::styled(
            format!("Remove `{entry}`?"),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[y] confirm   [n / Esc] cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    Paragraph::new(lines).render(modal_inner, frame.buffer_mut());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_parser_matches_09_to_17() {
        assert_eq!(parse_window("09:00-17:00"), Some((540, 1020)));
        assert_eq!(parse_window("bad"), None);
        assert_eq!(parse_window("25:00-26:00"), None);
    }

    #[test]
    fn classify_matches_the_three_regions() {
        // Core: 12:00.
        assert_eq!(classify(720, "09:00-17:00", 1), Availability::Core);
        // Shoulder: 08:15 (one hour before start).
        assert_eq!(
            classify(8 * 60 + 15, "09:00-17:00", 1),
            Availability::Shoulder
        );
        // Off: 03:00.
        assert_eq!(classify(180, "09:00-17:00", 1), Availability::Off);
    }

    #[test]
    fn search_zones_starts_before_contains() {
        let results = search_zones("shanghai");
        assert!(results.contains(&"Asia/Shanghai"));
        let results = search_zones("york");
        assert!(results.contains(&"America/New_York"));
    }

    #[test]
    fn map_dimensions_produce_landscape_two_to_one_visual() {
        // Cells are 4:1, giving 2:1 visual on typical 1:2-aspect terminals.
        // Width-bound: fill height at 4:1.
        assert_eq!(map_dimensions(120, 20), (80, 20));
        // Height-bound: cap width; drop rows to keep 4:1 cells.
        assert_eq!(map_dimensions(30, 40), (28, 7));
        // Exact 4:1 uses the whole rect.
        assert_eq!(map_dimensions(80, 20), (80, 20));
    }

    #[test]
    fn seeded_watchlist_loads_into_the_pane() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        let pane = ZonesPane::new(ZonesConfig::default(), path);
        assert!(!pane.list.entries.is_empty());
    }

    #[test]
    fn matching_entry_is_promoted_to_home_and_no_second_marker() {
        // Set up a pane where the home handle is known and appears in the
        // list. Exactly one marker MUST come back with is_home=true, and
        // no separate home marker is pushed alongside it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        let mut pane = ZonesPane::new(ZonesConfig::default(), path);
        // Overwrite the (possibly non-deterministic) auto-resolved home to
        // pin the test to Asia/Shanghai. Both the handle and one entry now
        // parse to `ZoneHandle::Named(Tz::Asia__Shanghai)`.
        pane.home = Some(parse_zone("Asia/Shanghai").unwrap());
        pane.list = ZoneList {
            entries: vec![
                ZoneEntry::new("America/New_York"),
                ZoneEntry::new("Asia/Shanghai"),
                ZoneEntry::new("Europe/London"),
            ],
        };
        let now = Utc::now();
        let markers = collect_markers(&pane, now, &subsolar(now));
        assert_eq!(markers.len(), 3, "one marker per entry, no duplicate");
        let homes: Vec<_> = markers.iter().filter(|m| m.is_home).collect();
        assert_eq!(homes.len(), 1, "exactly one home marker");
        assert_eq!(homes[0].color, Color::LightMagenta);
    }

    #[test]
    fn no_home_marker_when_home_zone_not_in_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        let mut pane = ZonesPane::new(ZonesConfig::default(), path);
        pane.home = Some(parse_zone("Antarctica/Vostok").unwrap());
        pane.list = ZoneList {
            entries: vec![ZoneEntry::new("America/New_York")],
        };
        let now = Utc::now();
        let markers = collect_markers(&pane, now, &subsolar(now));
        assert_eq!(markers.len(), 1);
        assert!(!markers[0].is_home);
    }

    #[test]
    fn duplicate_zones_collapse_to_one_marker() {
        // The bug: on a Shanghai-local box, `parse_zone("local")` and
        // `parse_zone("Asia/Shanghai")` both resolve to the same handle,
        // so both plot at the same map cell. `place_label` then paints
        // one time label to the RIGHT of ◉ and a second one to the LEFT.
        // After dedup only the first survives.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        let mut pane = ZonesPane::new(ZonesConfig::default(), path);
        pane.home = Some(parse_zone("Asia/Shanghai").unwrap());
        pane.list = ZoneList {
            entries: vec![
                // Two DIFFERENT input strings that resolve to the same handle.
                ZoneEntry::new("Asia/Shanghai"),
                ZoneEntry::new("Asia/Shanghai"),
                ZoneEntry::new("Europe/London"),
            ],
        };
        let now = Utc::now();
        let markers = collect_markers(&pane, now, &subsolar(now));
        // 2 markers, not 3 — the duplicate Shanghai row was dropped.
        assert_eq!(markers.len(), 2);
        assert_eq!(markers.iter().filter(|m| m.is_home).count(), 1);
    }
}
