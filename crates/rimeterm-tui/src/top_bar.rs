//! Single-row workspace header: menu + workspace tabs on the left,
//! status controls (`workspace:` / `shell:` / layout toggle / quit)
//! anchored to the right edge. See
//! `docs/plans/2026-09-29-single-row-workspace-header-design.md`.
//!
//! All row geometry is computed by one pure pass so the renderer and
//! the mouse router can never disagree about what sits in which cell.
//! [`render`] returns the hit rectangles for the exact frame it
//! painted; `App` caches them for `on_mouse`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use rimeterm_config::memory_state::WorkspaceLayoutMode;

use crate::workspace_activity::WorkspaceActivity;

pub use crate::workspace_strip::{WorkspaceHit, WorkspaceHover, WorkspaceRename};

/// Menu button ` ≡ rimeterm ` = 12 cells (unchanged from the old
/// status bar so the menu hit rect keeps its familiar size).
const MENU: &str = " ≡ rimeterm ";
/// Quit button ` [×]` = 4 cells.
const QUIT: &str = " [×]";
/// Separator between status segments: ` │ `.
const SEP: &str = " │ ";
/// Workspace tab separator: ` │ ` = 3 cells.
const TAB_SEP: &str = " │ ";
/// `[+]` new-workspace affordance: ` [+]"` = 4 cells.
const NEW_TAB: &str = " [+]";
/// Overflow scroll affordances: ` ‹` / `› ` = 2 cells each.
const OVERFLOW_PREV: &str = " ‹";
const OVERFLOW_NEXT: &str = "› ";
/// Full layout toggle labels.
const TOGGLE_LANDSCAPE: &str = " LANDSCAPE ";
const TOGGLE_VERTICAL: &str = " VERTICAL ";
/// Width-constrained toggle labels.
const TOGGLE_LANDSCAPE_SHORT: &str = " L ";
const TOGGLE_VERTICAL_SHORT: &str = " V ";
/// Head of the workspace segment: `workspace: `.
const WS_HEAD: &str = "workspace: ";
/// Cells reserved for the tab region before the status group starts
/// shrinking: a minimal ` x ` label + `× ` close = 5.
const TAB_REGION_MIN: u16 = 5;
/// Inactive-tab label cap (cells) tried before scrolling kicks in.
const INACTIVE_TITLE_CAP: u16 = 8;

/// Which status control the pointer is over (drives hover styling).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StatusBarHover {
    None,
    Menu,
    Landscape,
    Vertical,
    Quit,
}

/// Status-group hit rects consumed by `App` mouse routing.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusBarHits {
    pub menu: Option<Rect>,
    pub landscape: Option<Rect>,
    pub vertical: Option<Rect>,
    pub quit: Option<Rect>,
    /// `workspace:` segment rect (informational, not clickable).
    pub extra_first: Option<Rect>,
    /// `shell:` segment rect (informational, not clickable).
    pub extra_second: Option<Rect>,
}

/// Pure inputs to the header layout. No `App` access — keeps the
/// geometry unit-testable.
#[derive(Debug, Clone)]
pub struct TopBarInput<'a> {
    pub titles: &'a [String],
    pub activities: &'a [WorkspaceActivity],
    pub active: usize,
    pub workspace_label: &'a str,
    pub shell_short: &'a str,
    pub layout_mode: WorkspaceLayoutMode,
    pub tabs_enabled: bool,
    /// First logical tab index rendered in the tab region. `App` owns
    /// the offset so scrolling persists across frames; helpers here
    /// ([`ensure_active_visible`]) keep the active tab on screen.
    pub scroll_offset: usize,
    /// Viewer `F9 menu` chip — painted inside the middle gap only when
    /// the gap has spare cells.
    pub key_hint: Option<&'a str>,
}

/// Final status strings after the shrink ladder ran.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusText {
    /// `workspace: <label>` including truncation ellipsis; `None` when
    /// the segment was dropped entirely.
    pub workspace: Option<String>,
    /// `shell: <name>`; `None` when hidden by the shrink ladder.
    pub shell: Option<String>,
    /// `true` when the toggles render as ` L ` / ` V `.
    pub compact_toggles: bool,
    /// Hint chip actually painted (gap had room), if any.
    pub hint: Option<String>,
}

/// One frame's header geometry + hit rectangles.
#[derive(Debug, Clone, Default)]
pub struct TopBarHits {
    pub menu: Rect,
    /// Tab-label / close / `[+]` / overflow rects in visual order.
    pub workspace: Vec<(Rect, WorkspaceHit)>,
    pub status: StatusBarHits,
    /// Region owned by tabs (between menu and gap). Rename drafts are
    /// constrained to this rect.
    pub tabs_region: Rect,
    /// Middle flexible gap between tabs and the status group.
    pub gap: Rect,
    /// Hint chip rect when the gap had room for it.
    pub hint: Option<Rect>,
    /// Logical index range of rendered tabs.
    pub first_visible: Option<usize>,
    pub last_visible: Option<usize>,
    pub status_text: StatusText,
}

/// Draw the header row and return its hit rectangles.
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    input: &TopBarInput<'_>,
    ws_hover: WorkspaceHover,
    st_hover: StatusBarHover,
    rename: Option<&WorkspaceRename>,
) -> TopBarHits {
    let mut hits = TopBarHits::default();
    if area.width == 0 || area.height == 0 {
        return hits;
    }
    let lead = dw(MENU);
    let row_avail = area.width.saturating_sub(lead);
    let tab_min = if input.tabs_enabled && !input.titles.is_empty() {
        TAB_REGION_MIN
    } else {
        0
    };
    let status_avail = row_avail.saturating_sub(tab_min);

    // ---- status shrink ladder ----
    let status = shrink_status(status_avail, row_avail, input);
    let status_used = status.used;
    let status_x = area.x + area.width.saturating_sub(status_used);
    let tabs_region = Rect {
        x: area.x + lead,
        y: area.y,
        width: status_x.saturating_sub(area.x + lead),
        height: 1,
    };

    // ---- menu ----
    let menu_style = if matches!(st_hover, StatusBarHover::Menu) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Paragraph::new(MENU).style(menu_style).render(
        Rect {
            x: area.x,
            y: area.y,
            width: lead,
            height: 1,
        },
        buf,
    );
    hits.menu = Rect {
        x: area.x,
        y: area.y,
        width: lead,
        height: 1,
    };

    // ---- tabs ----
    let tabs = fit_tabs(tabs_region, input, rename);
    paint_tabs(&tabs, buf, input, ws_hover, rename);
    for e in &tabs.entries {
        if let Some(hit) = e.hit() {
            hits.workspace.push((e.rect(), hit));
        }
    }
    hits.first_visible = tabs.first_visible;
    hits.last_visible = tabs.last_visible;
    hits.tabs_region = tabs_region;

    // ---- gap (+ hint chip when it fits) ----
    let tabs_end = tabs
        .entries
        .last()
        .map(|e| e.x + e.w)
        .unwrap_or(tabs_region.x);
    hits.gap = Rect {
        x: tabs_end,
        y: area.y,
        width: status_x.saturating_sub(tabs_end),
        height: 1,
    };
    if let Some(hint) = input.key_hint {
        let chip = format!(" {hint} ");
        let chip_w = dw(&chip);
        if chip_w > 0 && chip_w <= hits.gap.width {
            let x = hits.gap.x + hits.gap.width - chip_w;
            Paragraph::new(chip.as_str())
                .style(Style::default().fg(Color::Cyan))
                .render(
                    Rect {
                        x,
                        y: area.y,
                        width: chip_w,
                        height: 1,
                    },
                    buf,
                );
            hits.hint = Some(Rect {
                x,
                y: area.y,
                width: chip_w,
                height: 1,
            });
            hits.status_text.hint = Some(chip);
        }
    }

    // ---- status group (right-aligned) ----
    let mut x = status_x;
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut paint = |text: &str, style: Style, w: u16, x: &mut u16| {
        if w > 0 {
            Paragraph::new(text).style(style).render(
                Rect {
                    x: *x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                buf,
            );
            *x += w;
        }
    };
    if let Some(ws) = &status.text.workspace {
        let w = dw(ws);
        paint(ws, Style::default(), w, &mut x);
        hits.status.extra_first = nonzero(Rect {
            x: x - w,
            y: area.y,
            width: w,
            height: 1,
        });
        paint(SEP, dim, dw(SEP), &mut x);
    }
    if let Some(shell) = &status.text.shell {
        let w = dw(shell);
        paint(shell, dim, w, &mut x);
        hits.status.extra_second = nonzero(Rect {
            x: x - w,
            y: area.y,
            width: w,
            height: 1,
        });
        paint(SEP, dim, dw(SEP), &mut x);
    }
    let (land_label, vert_label) = if status.text.compact_toggles {
        (TOGGLE_LANDSCAPE_SHORT, TOGGLE_VERTICAL_SHORT)
    } else {
        (TOGGLE_LANDSCAPE, TOGGLE_VERTICAL)
    };
    let land_w = dw(land_label);
    let land_style = toggle_style(
        input.layout_mode == WorkspaceLayoutMode::Landscape,
        matches!(st_hover, StatusBarHover::Landscape),
    );
    if let Some(style) = land_style {
        paint(land_label, style, land_w, &mut x);
        hits.status.landscape = nonzero(Rect {
            x: x - land_w,
            y: area.y,
            width: land_w,
            height: 1,
        });
    }
    let vert_w = dw(vert_label);
    let vert_style = toggle_style(
        input.layout_mode == WorkspaceLayoutMode::Vertical,
        matches!(st_hover, StatusBarHover::Vertical),
    );
    if let Some(style) = vert_style {
        paint(vert_label, style, vert_w, &mut x);
        hits.status.vertical = nonzero(Rect {
            x: x - vert_w,
            y: area.y,
            width: vert_w,
            height: 1,
        });
    }
    paint(SEP, dim, dw(SEP), &mut x);
    let quit_w = dw(QUIT);
    let quit_style = if matches!(st_hover, StatusBarHover::Quit) {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(Color::LightRed)
    };
    paint(QUIT, quit_style, quit_w, &mut x);
    hits.status.quit = nonzero(Rect {
        x: x - quit_w,
        y: area.y,
        width: quit_w,
        height: 1,
    });
    hits.status.menu = Some(hits.menu);
    hits.status_text.workspace = status.text.workspace.clone();
    hits.status_text.shell = status.text.shell.clone();
    hits.status_text.compact_toggles = status.text.compact_toggles;
    hits
}

/// Column of the rename editor's block caret: one cell past the draft
/// inside the tab region, clamped to the region's last cell.
pub fn rename_caret_col(area: Rect, input: &TopBarInput<'_>, rename: &WorkspaceRename) -> u16 {
    let region = tabs_region_for(area, input);
    let end = region.x + region.width;
    if region.width == 0 {
        return end.saturating_sub(1);
    }
    let tabs = fit_tabs(region, input, Some(rename));
    let mut x = region.x;
    for e in &tabs.entries {
        match e.kind {
            EntryKind::Tab(idx) => {
                if idx == rename.index {
                    let draft_w = dw(&rename.buffer);
                    let caret = e.x + 1 + draft_w;
                    return caret.min(end.saturating_sub(1));
                }
                x = e.x + e.w;
            }
            _ => {}
        }
    }
    let _ = x;
    end.saturating_sub(1)
}

/// Adjust `input.scroll_offset` so the active tab lands inside the
/// rendered range. Returns the offset `App` should store.
pub fn ensure_active_visible(area: Rect, input: &TopBarInput<'_>) -> usize {
    let region = tabs_region_for(area, input);
    let len = input.titles.len();
    if len == 0 || region.width == 0 {
        return 0;
    }
    // Smallest offset whose rendered range contains the active tab.
    let mut fallback = input.scroll_offset.min(len - 1);
    for offset in 0..len {
        let probe = TopBarInput {
            scroll_offset: offset,
            ..input.clone()
        };
        let tabs = fit_tabs(region, &probe, None);
        if let (Some(first), Some(last)) = (tabs.first_visible, tabs.last_visible) {
            if first <= input.active && input.active <= last {
                return offset;
            }
            if fallback == 0 && offset <= input.active {
                fallback = offset;
            }
        }
    }
    // Active can never fit as a full tab: make it the first rendered
    // (it will truncate) so it is at least partially on screen.
    input.active.min(len - 1)
}

// ---------------------------------------------------------------------------
// Geometry internals
// ---------------------------------------------------------------------------

/// Display width of `s` in terminal cells.
fn dw(s: &str) -> u16 {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s) as u16
}

/// Right-truncate `s` to `max` display cells with a trailing `…`.
fn truncate_cells(s: &str, max: u16) -> String {
    use unicode_width::UnicodeWidthChar;
    if max == 0 || dw(s) <= max {
        return s.to_owned();
    }
    if max == 1 {
        return "…".to_owned();
    }
    let mut width = 0u16;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if width + w > max - 1 {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

fn nonzero(r: Rect) -> Option<Rect> {
    if r.width == 0 || r.height == 0 {
        None
    } else {
        Some(r)
    }
}

fn toggle_style(selected: bool, hovered: bool) -> Option<Style> {
    let mut style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    if hovered && !selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Some(style)
}

/// Status-group shrink result.
struct ShrunkStatus {
    used: u16,
    text: StatusText,
}

/// Pick the first configuration that fits `avail` cells, per the
/// design ladder: full → workspace label truncated → toggles compact →
/// shell hidden → workspace dropped → shell dropped → toggles dropped.
/// Quit always keeps 4 cells whenever the row can spare them.
fn shrink_status(avail: u16, row_avail: u16, input: &TopBarInput<'_>) -> ShrunkStatus {
    let ws_full = format!("workspace: {}", input.workspace_label);
    let shell_full = format!("shell: {}", input.shell_short);
    let shell_w = dw(&shell_full);
    let sep = dw(SEP);
    let (land_full, vert_full) = (dw(TOGGLE_LANDSCAPE), dw(TOGGLE_VERTICAL));
    let (land_c, vert_c) = (dw(TOGGLE_LANDSCAPE_SHORT), dw(TOGGLE_VERTICAL_SHORT));
    let quit_w = dw(QUIT);

    // Rung helper: try a (ws, shell, toggles) configuration, returning
    // the built StatusText + used width when it fits.
    let try_cfg = |ws: Option<&str>, shell: bool, compact: bool| -> Option<(StatusText, u16)> {
        let (lw, lv) = if compact {
            (land_c, vert_c)
        } else {
            (land_full, vert_full)
        };
        let toggles = lw + lv;
        let mut text = StatusText {
            compact_toggles: compact,
            ..Default::default()
        };
        // Components in order ws+sep, shell+sep, toggles, sep, quit.
        let mut u = 0u16;
        if let Some(ws_text) = ws {
            u += dw(ws_text) + sep;
            text.workspace = Some(ws_text.to_owned());
        }
        if shell {
            u += shell_w + sep;
            text.shell = Some(shell_full.clone());
        }
        u += toggles + sep + quit_w;
        if u <= avail { Some((text, u)) } else { None }
    };

    // Rung 1: everything full.
    if let Some((text, used)) = try_cfg(Some(&ws_full), true, false) {
        return ShrunkStatus { used, text };
    }
    // Rung 2-4: workspace label truncated (head + as much label as fits).
    let trunc_ws = |rest: u16| -> Option<String> {
        let label_budget = avail.saturating_sub(rest + dw(WS_HEAD));
        if label_budget == 0 {
            return None;
        }
        Some(format!(
            "{WS_HEAD}{}",
            truncate_cells(input.workspace_label, label_budget)
        ))
    };
    // Rung 2: truncated ws + shell + full toggles.
    let rest2 = sep + shell_w + sep + land_full + vert_full + sep + quit_w;
    if let Some(ws) = trunc_ws(rest2)
        && let Some((text, used)) = try_cfg(Some(&ws), true, false)
    {
        return ShrunkStatus { used, text };
    }
    // Rung 3: truncated ws + shell + compact toggles.
    let rest3 = sep + shell_w + sep + land_c + vert_c + sep + quit_w;
    if let Some(ws) = trunc_ws(rest3)
        && let Some((text, used)) = try_cfg(Some(&ws), true, true)
    {
        return ShrunkStatus { used, text };
    }
    // Rung 4: truncated ws + compact toggles (shell hidden).
    let rest4 = sep + land_c + vert_c + sep + quit_w;
    if let Some(ws) = trunc_ws(rest4)
        && let Some((text, used)) = try_cfg(Some(&ws), false, true)
    {
        return ShrunkStatus { used, text };
    }
    // Rung 5: shell + compact toggles (workspace dropped).
    if let Some((text, used)) = try_cfg(None, true, true) {
        return ShrunkStatus { used, text };
    }
    // Rung 6: compact toggles only.
    if let Some((text, used)) = try_cfg(None, false, true) {
        return ShrunkStatus { used, text };
    }
    // Rung 7: quit alone — steal from the tab region if the row can
    // spare 4 cells anywhere.
    if quit_w <= row_avail {
        return ShrunkStatus {
            used: quit_w,
            text: StatusText::default(),
        };
    }
    ShrunkStatus {
        used: 0,
        text: StatusText::default(),
    }
}

// ---------------------------------------------------------------------------
// Tab region layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    OverflowPrev,
    Tab(usize),
    Close(usize),
    Sep,
    OverflowNext,
    New,
}

#[derive(Debug, Clone)]
struct TabEntry {
    kind: EntryKind,
    /// Tab label text (styled separately from close affordance).
    label: Option<String>,
    x: u16,
    w: u16,
}

impl TabEntry {
    fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: 0,
            width: self.w,
            height: 1,
        }
    }
    fn hit(&self) -> Option<WorkspaceHit> {
        match self.kind {
            EntryKind::OverflowPrev => Some(WorkspaceHit::OverflowPrev),
            EntryKind::Tab(i) => Some(WorkspaceHit::Tab(i)),
            EntryKind::Close(i) => Some(WorkspaceHit::Close(i)),
            EntryKind::OverflowNext => Some(WorkspaceHit::OverflowNext),
            EntryKind::New => Some(WorkspaceHit::New),
            EntryKind::Sep => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TabLayout {
    entries: Vec<TabEntry>,
    first_visible: Option<usize>,
    last_visible: Option<usize>,
}

/// Natural width of a rendered tab label plus its close affordance.
fn tab_natural(label: &str) -> u16 {
    dw(label) + 2
}

fn activity_for(input: &TopBarInput<'_>, idx: usize) -> WorkspaceActivity {
    input.activities.get(idx).copied().unwrap_or_default()
}

fn activity_title(input: &TopBarInput<'_>, idx: usize, title: &str) -> String {
    match activity_for(input, idx).glyph() {
        Some(glyph) => format!("{glyph} {title}"),
        None => title.to_string(),
    }
}

fn tab_label(input: &TopBarInput<'_>, idx: usize, title: &str, renaming: bool) -> String {
    let title = activity_title(input, idx, title);
    if renaming {
        format!(" {title}")
    } else {
        format!(" {title} ")
    }
}

/// Fit the workspace tabs into `region`, applying the shrink ladder:
/// natural → inactive titles capped → scroll with `‹ ›` affordances.
fn fit_tabs(region: Rect, input: &TopBarInput<'_>, rename: Option<&WorkspaceRename>) -> TabLayout {
    let mut layout = TabLayout::default();
    if !input.tabs_enabled || input.titles.is_empty() || region.width == 0 {
        return layout;
    }
    let len = input.titles.len();
    let offset = input.scroll_offset.min(len - 1);

    // Total width with every rendered tab label at `natural` (rename draft
    // replaces the renamed tab's title).
    let title_of = |idx: usize| -> String {
        if let Some(r) = rename
            && r.index == idx
        {
            r.buffer.clone()
        } else {
            input.titles[idx].clone()
        }
    };
    let natural_total = |cap: Option<u16>| -> u16 {
        let mut total = dw(NEW_TAB);
        for idx in 0..len {
            let title = if cap.is_some() && idx != input.active {
                truncate_cells(&title_of(idx), cap.unwrap_or(u16::MAX))
            } else {
                title_of(idx)
            };
            let renaming = rename.is_some_and(|r| r.index == idx);
            total += tab_natural(&tab_label(input, idx, &title, renaming));
        }
        total + dw(TAB_SEP) * (len.saturating_sub(1)) as u16
    };
    if natural_total(None) <= region.width {
        push_all(&mut layout, region, input, rename, None, offset);
        return layout;
    }
    if len > 1 && natural_total(Some(INACTIVE_TITLE_CAP)) <= region.width {
        push_all(
            &mut layout,
            region,
            input,
            rename,
            Some(INACTIVE_TITLE_CAP),
            offset,
        );
        return layout;
    }

    // Scroll: walk from `offset`, reserving `[+]` and (two-pass) `›`.
    for reserve_next in [false, true] {
        layout = TabLayout::default();
        let mut x = region.x;
        let show_prev = offset > 0 && region.width >= 8;
        if show_prev {
            layout.entries.push(TabEntry {
                kind: EntryKind::OverflowPrev,
                label: None,
                x,
                w: dw(OVERFLOW_PREV),
            });
            x += dw(OVERFLOW_PREV);
        }
        let reserve = dw(NEW_TAB) + if reserve_next { dw(OVERFLOW_NEXT) } else { 0 };
        let avail_end = region.x + region.width.saturating_sub(reserve);
        let mut last_idx = None;
        let mut first_idx = None;
        for idx in offset..len {
            let title = title_of(idx);
            let renaming = rename.is_some_and(|r| r.index == idx);
            let label = tab_label(input, idx, &title, renaming);
            let sep_w = if last_idx.is_some() { dw(TAB_SEP) } else { 0 };
            let natural = tab_natural(&label);
            if x + sep_w + natural <= avail_end {
                if last_idx.is_some() {
                    layout.entries.push(TabEntry {
                        kind: EntryKind::Sep,
                        label: None,
                        x,
                        w: sep_w,
                    });
                    x += sep_w;
                }
                let lw = dw(&label);
                layout.entries.push(TabEntry {
                    kind: EntryKind::Tab(idx),
                    label: Some(label),
                    x,
                    w: lw,
                });
                x += lw;
                layout.entries.push(TabEntry {
                    kind: EntryKind::Close(idx),
                    label: None,
                    x,
                    w: 2,
                });
                x += 2;
                first_idx = first_idx.or(Some(idx));
                last_idx = Some(idx);
            } else {
                // Try truncating this tab to what's left (keeping at
                // least ` x… ` + close), then stop.
                let room = avail_end.saturating_sub(x + sep_w);
                let min_w = 3 + 2; // ` x… ` + close
                if room >= min_w {
                    if last_idx.is_some() {
                        layout.entries.push(TabEntry {
                            kind: EntryKind::Sep,
                            label: None,
                            x,
                            w: sep_w,
                        });
                        x += sep_w;
                    }
                    let budget = room.saturating_sub(2 + 2 + 1); // close `× ` + padding + …-space
                    let short_title = truncate_cells(&title, budget.max(1));
                    let label = tab_label(input, idx, &short_title, renaming);
                    let lw = dw(&label);
                    layout.entries.push(TabEntry {
                        kind: EntryKind::Tab(idx),
                        label: Some(label),
                        x,
                        w: lw,
                    });
                    x += lw;
                    layout.entries.push(TabEntry {
                        kind: EntryKind::Close(idx),
                        label: None,
                        x,
                        w: 2,
                    });
                    x += 2;
                    first_idx = first_idx.or(Some(idx));
                    last_idx = Some(idx);
                }
                break;
            }
        }
        // Force-render the first tab (truncated) when nothing fit and
        // the region can show a minimal ` x… ×` — the active/first tab
        // outranks the `[+]` reservation.
        if first_idx.is_none() {
            let room = region
                .width
                .saturating_sub(if show_prev { dw(OVERFLOW_PREV) } else { 0 });
            if room >= 5 {
                let title = title_of(offset);
                let renaming = rename.is_some_and(|r| r.index == offset);
                let budget = room.saturating_sub(3);
                let short_title = truncate_cells(&title, budget.max(1));
                let label = tab_label(input, offset, &short_title, renaming);
                let lw = dw(&label);
                let entry_x = if show_prev {
                    region.x + dw(OVERFLOW_PREV)
                } else {
                    region.x
                };
                layout.entries.push(TabEntry {
                    kind: EntryKind::Tab(offset),
                    label: Some(label),
                    x: entry_x,
                    w: lw,
                });
                layout.entries.push(TabEntry {
                    kind: EntryKind::Close(offset),
                    label: None,
                    x: entry_x + lw,
                    w: 2,
                });
                first_idx = Some(offset);
                last_idx = Some(offset);
                x = entry_x + lw + 2;
            }
        }
        let leftover = match last_idx {
            Some(last) => last + 1 < len,
            None => false,
        };
        if !leftover || reserve_next {
            if leftover && reserve_next {
                layout.entries.push(TabEntry {
                    kind: EntryKind::OverflowNext,
                    label: None,
                    x,
                    w: dw(OVERFLOW_NEXT),
                });
                x += dw(OVERFLOW_NEXT);
            }
            if x + dw(NEW_TAB) <= region.x + region.width || !leftover {
                layout.entries.push(TabEntry {
                    kind: EntryKind::New,
                    label: None,
                    x,
                    w: dw(NEW_TAB),
                });
            }
            layout.first_visible = first_idx;
            layout.last_visible = last_idx;
            // Clip entries to the region (no phantom hits).
            layout
                .entries
                .retain_mut(|e| e.x + e.w <= region.x + region.width);
            return layout;
        }
    }
    layout
}

/// Push every tab (optionally inactive-capped) + `[+]` — the
/// everything-fits path.
fn push_all(
    layout: &mut TabLayout,
    region: Rect,
    input: &TopBarInput<'_>,
    rename: Option<&WorkspaceRename>,
    cap: Option<u16>,
    _offset: usize,
) {
    let mut x = region.x;
    for idx in 0..input.titles.len() {
        let title = if let Some(r) = rename
            && r.index == idx
        {
            r.buffer.clone()
        } else if cap.is_some() && idx != input.active {
            truncate_cells(&input.titles[idx], cap.unwrap_or(u16::MAX))
        } else {
            input.titles[idx].clone()
        };
        if idx > 0 {
            layout.entries.push(TabEntry {
                kind: EntryKind::Sep,
                label: None,
                x,
                w: dw(TAB_SEP),
            });
            x += dw(TAB_SEP);
        }
        let label = tab_label(input, idx, &title, rename.is_some_and(|r| r.index == idx));
        let lw = dw(&label);
        layout.entries.push(TabEntry {
            kind: EntryKind::Tab(idx),
            label: Some(label),
            x,
            w: lw,
        });
        x += lw;
        layout.entries.push(TabEntry {
            kind: EntryKind::Close(idx),
            label: None,
            x,
            w: 2,
        });
        x += 2;
    }
    layout.entries.push(TabEntry {
        kind: EntryKind::New,
        label: None,
        x,
        w: dw(NEW_TAB),
    });
    layout.first_visible = Some(0);
    layout.last_visible = Some(input.titles.len().saturating_sub(1));
    let _ = cap;
}

fn paint_tabs(
    layout: &TabLayout,
    buf: &mut Buffer,
    input: &TopBarInput<'_>,
    hover: WorkspaceHover,
    rename: Option<&WorkspaceRename>,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut cursor = layout.entries.first().map(|e| e.x).unwrap_or(0);
    for e in &layout.entries {
        if e.x > cursor {
            spans.push(Span::raw(" ".repeat((e.x - cursor) as usize)));
        }
        match e.kind {
            EntryKind::Sep => spans.push(Span::styled(TAB_SEP, dim)),
            EntryKind::OverflowPrev => {
                let style = if matches!(hover, WorkspaceHover::OverflowPrev) {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    dim
                };
                spans.push(Span::styled(OVERFLOW_PREV, style));
            }
            EntryKind::OverflowNext => {
                let style = if matches!(hover, WorkspaceHover::OverflowNext) {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    dim
                };
                spans.push(Span::styled(OVERFLOW_NEXT, style));
            }
            EntryKind::New => {
                let style = if matches!(hover, WorkspaceHover::New) {
                    Style::default()
                } else {
                    dim
                };
                spans.push(Span::styled(NEW_TAB, style));
            }
            EntryKind::Tab(idx) => {
                let label = e.label.clone().unwrap_or_default();
                let is_active = idx == input.active;
                let is_renaming = rename.is_some_and(|r| r.index == idx);
                let style = if is_renaming || is_active {
                    let mut style = Style::default().add_modifier(Modifier::REVERSED);
                    if matches!(hover, WorkspaceHover::Tab(i) if i == idx) {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    style
                } else {
                    let mut style = Style::default();
                    if matches!(hover, WorkspaceHover::Tab(i) if i == idx) {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    style
                };
                spans.push(Span::styled(label, style));
            }
            EntryKind::Close(_idx) => {
                let mut style = dim;
                if matches!(hover, WorkspaceHover::Close(i) if i == _idx) {
                    style = style.fg(Color::LightRed).add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled("× ", style));
            }
        }
        cursor = e.x + e.w;
    }
    if let Some(first) = layout.entries.first() {
        Paragraph::new(Line::from(spans)).render(
            Rect {
                x: first.x,
                y: input_row_y(layout),
                width: cursor.saturating_sub(first.x),
                height: 1,
            },
            buf,
        );
    }
}

fn input_row_y(_layout: &TabLayout) -> u16 {
    0
}

fn tabs_region_for(area: Rect, input: &TopBarInput<'_>) -> Rect {
    if area.width == 0 {
        return Rect::default();
    }
    let lead = dw(MENU);
    let row_avail = area.width.saturating_sub(lead);
    let tab_min = if input.tabs_enabled && !input.titles.is_empty() {
        TAB_REGION_MIN
    } else {
        0
    };
    let status = shrink_status(row_avail.saturating_sub(tab_min), row_avail, input);
    let status_x = area.x + area.width.saturating_sub(status.used);
    Rect {
        x: area.x + lead,
        y: area.y,
        width: status_x.saturating_sub(area.x + lead),
        height: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn titles() -> Vec<String> {
        vec!["alpha".into(), "beta".into()]
    }

    fn input(titles: &[String]) -> TopBarInput<'_> {
        TopBarInput {
            titles,
            activities: &[],
            active: 0,
            workspace_label: "myproj",
            shell_short: "pwsh",
            layout_mode: WorkspaceLayoutMode::Landscape,
            tabs_enabled: true,
            scroll_offset: 0,
            key_hint: None,
        }
    }

    fn row(width: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        }
    }

    fn draw_to_string(width: u16, input: &TopBarInput<'_>) -> (String, TopBarHits) {
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        let mut hits = TopBarHits::default();
        term.draw(|f| {
            hits = render(
                f.area(),
                f.buffer_mut(),
                input,
                WorkspaceHover::None,
                StatusBarHover::None,
                None,
            );
        })
        .unwrap();
        let text: String = (0..width)
            .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        (text, hits)
    }

    // Status group reference widths (myproj/pwsh):
    //   ws 17 + 3 + shell 11 + 3 + 11 + 10 + 3 + quit 4 = 62.
    const STATUS_FULL: u16 = 62;

    #[test]
    fn wide_row_puts_status_group_flush_right() {
        let (_, hits) = draw_to_string(100, &input(&titles()));
        let quit = hits.status.quit.expect("quit rect");
        assert_eq!(quit.x + quit.width, 100, "quit must end at the edge");
        assert_eq!(hits.menu.x, 0);
        assert_eq!(hits.menu.width, dw(MENU));
        // Status group segments are tightly packed with ` │ ` between.
        let ws = hits.status.extra_first.expect("ws rect");
        let shell = hits.status.extra_second.expect("shell rect");
        let land = hits.status.landscape.expect("landscape rect");
        let vert = hits.status.vertical.expect("vertical rect");
        assert_eq!(shell.x, ws.x + ws.width + dw(SEP));
        assert_eq!(land.x, shell.x + shell.width + dw(SEP));
        assert_eq!(vert.x, land.x + land.width, "toggles adjacent");
        assert_eq!(quit.x, vert.x + vert.width + dw(SEP));
    }

    #[test]
    fn no_duplicated_rimeterm_workspace_prefix() {
        let (text, hits) = draw_to_string(100, &input(&titles()));
        assert!(
            !text.contains("workspace: alpha"),
            "tabs must not carry the old prefix: {text}"
        );
        assert_eq!(
            text.matches("rimeterm").count(),
            1,
            "only the menu names the app: {text}"
        );
        assert!(text.contains("alpha"));
        assert!(text.contains("beta"));
        assert!(text.contains("[+]"));
        assert!(
            hits.workspace
                .iter()
                .any(|(_, h)| matches!(h, WorkspaceHit::New))
        );
    }
    #[test]
    fn inactive_tabs_show_status_glyphs_and_active_tab_stays_reversed() {
        let t = titles();
        let activities = [WorkspaceActivity::Failed, WorkspaceActivity::Working];
        let inp = TopBarInput {
            titles: &t,
            activities: &activities,
            ..input(&t)
        };
        let (text, _) = draw_to_string(100, &inp);
        assert!(
            text.contains("! alpha"),
            "active status glyph is visible: {text}"
        );
        assert!(
            text.contains("● be"),
            "inactive status glyph is visible: {text}"
        );
    }
    #[test]
    fn current_tab_keeps_reverse_video_while_inactive_tab_uses_plain_background() {
        let t = titles();
        let activities = [WorkspaceActivity::Failed, WorkspaceActivity::Working];
        let inp = TopBarInput {
            titles: &t,
            activities: &activities,
            ..input(&t)
        };
        let mut term = Terminal::new(TestBackend::new(100, 1)).unwrap();
        let mut hits = TopBarHits::default();
        term.draw(|f| {
            hits = render(
                f.area(),
                f.buffer_mut(),
                &inp,
                WorkspaceHover::None,
                StatusBarHover::None,
                None,
            );
        })
        .unwrap();
        let active_rect = hits
            .workspace
            .iter()
            .find(|(_, hit)| matches!(hit, WorkspaceHit::Tab(0)))
            .map(|(rect, _)| *rect)
            .unwrap();
        let inactive_rect = hits
            .workspace
            .iter()
            .find(|(_, hit)| matches!(hit, WorkspaceHit::Tab(1)))
            .map(|(rect, _)| *rect)
            .unwrap();
        let buffer = term.backend().buffer();
        let active_style = buffer[(active_rect.x, 0)].style();
        let inactive_style = buffer[(inactive_rect.x, 0)].style();
        assert!(active_style.add_modifier.contains(Modifier::REVERSED));
        assert_ne!(active_style.bg, Some(Color::Red));
        assert_eq!(inactive_style.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn activity_label_width_includes_glyph() {
        let t = titles();
        let quiet = input(&t);
        let active = TopBarInput {
            activities: &[WorkspaceActivity::Quiet, WorkspaceActivity::NeedsInput],
            ..quiet.clone()
        };
        let (_, quiet_hits) = draw_to_string(100, &quiet);
        let (_, active_hits) = draw_to_string(100, &active);
        let quiet_beta = quiet_hits
            .workspace
            .iter()
            .find(|(_, hit)| matches!(hit, WorkspaceHit::Tab(1)))
            .map(|(rect, _)| rect.width)
            .unwrap();
        let active_beta = active_hits
            .workspace
            .iter()
            .find(|(_, hit)| matches!(hit, WorkspaceHit::Tab(1)))
            .map(|(rect, _)| rect.width)
            .unwrap();
        assert_eq!(active_beta, quiet_beta + 2);
    }

    #[test]
    fn wide_gap_sits_between_tabs_and_status() {
        let (_, hits) = draw_to_string(100, &input(&titles()));
        let last_tab_end = hits
            .workspace
            .iter()
            .map(|(r, _)| r.x + r.width)
            .max()
            .expect("tab affordances");
        let status_start = hits
            .status
            .extra_first
            .or(hits.status.landscape)
            .expect("status origin")
            .x;
        assert!(hits.gap.width > 0, "wide rows keep a flexible gap");
        assert_eq!(hits.gap.x, last_tab_end);
        assert_eq!(hits.gap.x + hits.gap.width, status_start);
    }

    #[test]
    fn shrink_ladder_truncates_then_compacts_then_hides() {
        // (a) Everything fits (menu 12 + tabs 24 + status 62 = 98).
        let (t, h) = draw_to_string(110, &input(&titles()));
        assert!(t.contains("workspace: myproj"), "(a) full ws: {t}");
        assert!(t.contains("shell: pwsh"));
        assert!(!h.status_text.compact_toggles);
        // (b) status_avail 58: ws truncates, shell + full toggles stay.
        let (t, h) = draw_to_string(75, &input(&titles()));
        assert!(t.contains("workspace: m…"), "(b) ws truncated: {t}");
        assert!(t.contains("shell: pwsh"), "(b) shell kept: {t}");
        assert!(!h.status_text.compact_toggles, "(b) full toggles: {t}");
        // (c) status_avail 43: toggles compact, shell still visible.
        let (t, h) = draw_to_string(60, &input(&titles()));
        assert!(h.status_text.compact_toggles, "(c) compact at 60: {t}");
        assert!(t.contains(" L "));
        assert!(t.contains("shell: pwsh"), "(c) shell kept: {t}");
        // (d) status_avail 33: shell hidden, ws + compact toggles stay.
        let (t, h) = draw_to_string(50, &input(&titles()));
        assert!(h.status_text.shell.is_none(), "(d) shell hidden at 50: {t}");
        assert!(t.contains("workspace: myproj"), "(d) ws kept: {t}");
        assert!(h.status.landscape.is_some());
        assert!(h.status.vertical.is_some());
        assert!(h.status.quit.is_some());
        // (e) status_avail 17: ws dropped too, compact toggles remain.
        let (t, h) = draw_to_string(34, &input(&titles()));
        assert!(
            h.status_text.workspace.is_none(),
            "(e) ws dropped at 34: {t}"
        );
        assert!(h.status_text.shell.is_none());
        assert!(h.status.landscape.is_some());
        assert!(h.status.vertical.is_some());
        assert!(h.status.quit.is_some());
        let _ = STATUS_FULL;
    }

    #[test]
    fn narrow_rows_never_overlap() {
        for width in [20u16, 26, 30, 34, 40, 46, 52, 60] {
            let (_, hits) = draw_to_string(width, &input(&titles()));
            let mut rects: Vec<Rect> = vec![hits.menu];
            rects.extend(hits.workspace.iter().map(|(r, _)| *r));
            for r in [
                hits.status.landscape,
                hits.status.vertical,
                hits.status.quit,
            ]
            .into_iter()
            .flatten()
            {
                rects.push(r);
            }
            rects.retain(|r| r.width > 0);
            for i in 0..rects.len() {
                for j in i + 1..rects.len() {
                    let (a, b) = (rects[i], rects[j]);
                    assert!(
                        a.x + a.width <= b.x || b.x + b.width <= a.x,
                        "overlap at width {width}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn tabs_scroll_with_overflow_affordances() {
        let many: Vec<String> = (0..8).map(|i| format!("tab{i}")).collect();
        let inp = input(&many);
        let (text, hits) = draw_to_string(104, &inp);
        assert!(
            hits.workspace
                .iter()
                .any(|(_, h)| matches!(h, WorkspaceHit::OverflowNext)),
            "overflow-next expected at 104 cols: {text}"
        );
        assert_eq!(hits.first_visible, Some(0));
        assert!(hits.last_visible.unwrap() < many.len() - 1);
        // Scrolling forward reveals later tabs and shows `‹`.
        let scrolled = TopBarInput {
            scroll_offset: 3,
            ..inp.clone()
        };
        let (text2, hits2) = draw_to_string(104, &scrolled);
        assert!(
            hits2
                .workspace
                .iter()
                .any(|(_, h)| matches!(h, WorkspaceHit::OverflowPrev)),
            "overflow-prev expected when offset>0: {text2}"
        );
        assert_eq!(hits2.first_visible, Some(3));
    }

    #[test]
    fn ensure_active_visible_pulls_offset_forward() {
        let many: Vec<String> = (0..8).map(|i| format!("tab{i}")).collect();
        let inp = input(&many);
        // Wide row: offset 0 already shows tabs 0-1; active 6 needs a
        // forward shift.
        let offset = ensure_active_visible(
            row(104),
            &TopBarInput {
                active: 6,
                ..inp.clone()
            },
        );
        let shifted = TopBarInput {
            scroll_offset: offset,
            ..inp.clone()
        };
        let (_, hits) = draw_to_string(104, &shifted);
        assert!(
            hits.first_visible.unwrap() <= 6 && 6 <= hits.last_visible.unwrap(),
            "active 6 must be visible with offset {offset}: {:?}..{:?}",
            hits.first_visible,
            hits.last_visible
        );
    }

    #[test]
    fn single_huge_title_truncates_to_region() {
        let huge = vec!["x".repeat(80)];
        let inp = input(&huge);
        let (text, hits) = draw_to_string(40, &inp);
        assert!(text.contains('…'), "huge title must truncate: {text}");
        assert!(hits.tabs_region.x + hits.tabs_region.width <= 40);
        assert!(hits.status.quit.is_some(), "quit stays reachable");
    }

    #[test]
    fn disabled_tabs_render_status_only() {
        let t = titles();
        let mut inp = input(&t);
        inp.tabs_enabled = false;
        let (text, hits) = draw_to_string(80, &inp);
        assert!(!text.contains("alpha"), "no tabs when disabled: {text}");
        assert!(hits.workspace.is_empty());
        assert!(!text.contains("[+]"));
        assert_eq!(hits.status.quit.expect("quit").x + dw(QUIT), 80);
    }

    #[test]
    fn hint_chip_uses_gap_only() {
        let t = titles();
        let mut inp = input(&t);
        inp.key_hint = Some("F9 menu");
        let (text, hits) = draw_to_string(120, &inp);
        assert!(text.contains("F9 menu"), "chip fits at 120: {text}");
        let hint = hits.hint.expect("hint rect");
        assert!(hint.x >= hits.gap.x);
        assert!(hint.x + hint.width <= hits.gap.x + hits.gap.width);
        // Narrow: no gap → no chip, status untouched.
        let (text, _) = draw_to_string(46, &inp);
        assert!(
            !text.contains("F9 menu"),
            "chip must vanish with the gap: {text}"
        );
    }

    // --- rename caret ---------------------------------------------------

    #[test]
    fn rename_caret_walks_and_clamps_to_tab_region() {
        let t = titles();
        let inp = input(&t);
        let r0 = WorkspaceRename {
            index: 0,
            buffer: "ab".into(),
        };
        let col = rename_caret_col(row(80), &inp, &r0);
        let (_, hits) = draw_to_string(80, &inp);
        assert!(
            col >= hits.tabs_region.x && col < hits.tabs_region.x + hits.tabs_region.width,
            "caret {col} inside tab region {:?}",
            hits.tabs_region
        );
        // Over-long draft clamps to the region's last cell, never into
        // the status group.
        let huge_draft = WorkspaceRename {
            index: 0,
            buffer: "x".repeat(60),
        };
        let clamped = rename_caret_col(row(80), &inp, &huge_draft);
        assert!(clamped < hits.tabs_region.x + hits.tabs_region.width);
    }
}
