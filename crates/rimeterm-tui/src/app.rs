//! M2 App wiring — the four-quadrant workspace.
//!
//! Layout matches §19.1 (viewer folded state):
//!
//! ```text
//! ┌ ≡ rimeterm ─── workspace ─── shell: pwsh 7 ─┐
//! │ ┤ files ├ …           │ ┤ agents ├ …       │
//! │  (yazi/gitui)         │  (omp/pi/…)        │
//! ├───────────────────────┼────────────────────┤
//! │ ┤ sysmon ├ …          │ ┤ shells ├ …       │
//! │  (bottom/…)           │  (shell-1)         │
//! └ hint bar ──────────────────────────────────┘
//! ```
//!
//! Real plugin providers land in later milestones; every non-shell cell shows
//! a `PlaceholderPane` labeled with the group's active tab.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{Event, EventStream, KeyEvent, KeyEventKind};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use rimeterm_config::Config;
use rimeterm_core::app_menu::AppMenu;
use rimeterm_core::command::{Command, CommandRegistry};
use rimeterm_core::event::EventBus;
use rimeterm_core::focus::FocusManager;
use rimeterm_core::layout::{LayoutNode, LayoutTree};
use rimeterm_core::pane::{PaneId, PaneProvider, PaneRenderCtx};
use rimeterm_core::tabs::{
    BUILTIN_AGENTS, BUILTIN_FILES, BUILTIN_SHELLS, BUILTIN_SYSMON, MembersPolicy, PaneKind,
    TabGroup, TabGroupId,
};
use rimeterm_pty::{ShellChoice, detect_default_shell};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::keymap::{Keymap, KeymapOutcome, QUADRANT_COMMANDS, tab_goto_command_id};
use crate::menu::{
    MenuKeyOutcome, MenuState, handle_key as menu_key, popup_rect as menu_rect,
    render as render_menu,
};
use crate::palette::{
    CommandEntry, PaletteOutcome, PaletteState, handle_key as palette_key,
    popup_rect as palette_rect, render as render_palette,
};
use crate::pane_registry::PaneRegistry;
use crate::placeholder_pane::PlaceholderPane;
use crate::shell_factory::spawn_shell;
use crate::status_bar::render as render_status_bar;
use crate::tab_strip::render as render_tab_strip;
use crate::terminal::TerminalGuard;

/// Pending command actions the app main loop resolves outside command bodies.
#[derive(Debug, Default)]
struct ActionFlags {
    quit: AtomicBool,
    menu_toggle: AtomicBool,
    palette_toggle: AtomicBool,
    shells_new: AtomicBool,
    shells_close: AtomicBool,
    tab_next: AtomicBool,
    tab_prev: AtomicBool,
    tab_goto: AtomicUsize,       // 1..=9 = goto; 0 = idle.
    focus_dir: AtomicUsize,      // 1=left 2=right 3=up 4=down; 0 = idle.
    focus_quadrant: AtomicUsize, // 1..=4; 0 = idle.
    settings: AtomicBool,
    resize_toggle: AtomicBool,
    layout_reset: AtomicBool,
    acknowledgement: AtomicBool,
}

/// A mutation the IPC handler queues for the main loop. Each variant carries
/// a oneshot sender so the caller learns whether the mutation applied and
/// (for `OpenShell`) which `PaneId` was created.
///
/// Why a queue instead of a flag? For `pane.close` / `pane.open` the caller
/// needs the outcome **synchronously** (was there such a pane? what's the
/// new id?), and mutation must happen on the App's owning thread — flags
/// only fit fire-and-forget signals like `Ctrl+T`.
pub(crate) enum PaneMutation {
    /// Close whichever tab holds `pane_id`. Ack: `Ok(())` on success, or the
    /// policy error string.
    Close {
        pane_id: PaneId,
        ack: std::sync::mpsc::SyncSender<Result<(), String>>,
    },
    /// Open a new shell tab in the shells group. Ack: `Ok(new_pane_id)` or
    /// error. `kind` is validated by the command layer; the enum only carries
    /// what the App can act on today.
    OpenShell {
        ack: std::sync::mpsc::SyncSender<Result<u64, String>>,
    },
    /// Open a new agent tab (BUILTIN_AGENTS). `spec` is a static ref into
    /// [`rimeterm_pty::agent_registry::AGENT_REGISTRY`]; `parse_open_args`
    /// already verified membership.
    OpenAgent {
        spec: &'static rimeterm_pty::agent_registry::AgentSpec,
        ack: std::sync::mpsc::SyncSender<Result<u64, String>>,
    },
    /// Rename any pane in place. Ack: `Ok(())` if the provider accepted the
    /// new title; `Err(_)` if the pane wasn't found or the provider refused
    /// (currently only Native panes without a mutable title, none of which
    /// exist yet).
    Rename {
        pane_id: PaneId,
        title: String,
        ack: std::sync::mpsc::SyncSender<Result<(), String>>,
    },
    /// Focus any pane. Ack: `Ok(())` if the pane exists; error otherwise.
    /// Activates the pane's tab inside its owning group as a side effect.
    Focus {
        pane_id: PaneId,
        ack: std::sync::mpsc::SyncSender<Result<(), String>>,
    },
    /// Open a new shell tab and immediately type `command` into it (no
    /// Enter — the user reviews + confirms). Fire-and-forget: keymap
    /// dispatch doesn't wait for the new pane id, and the shell handles
    /// its own errors visibly. Used by the placeholder `[I]` shortcut.
    OpenShellAndType { command: String },
}

/// Title of the placeholder pane that seeds the `agents` group on first
/// launch (see §14 / C14). We match on this string in `new_agent_tab_in`
/// to auto-close the picker once the first real agent tab lands.
pub(crate) const AGENT_PICKER_TITLE: &str = "Pick an agent";

/// Result of hit-testing a mouse click against the cached tab-strip rects.
/// Emitted by [`App::tab_hit`] and consumed by [`App::on_mouse`]; kept as
/// an enum (rather than a tuple) so each arm names its intent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TabStripHit {
    /// Click on a tab label — activate that tab.
    Activate { gid: TabGroupId, idx: usize },
    /// Click on the `×` — close that tab.
    Close { gid: TabGroupId, idx: usize },
    /// Click on the `[+]` — dispatch new-tab for that group.
    Plus { gid: TabGroupId },
}

/// Snapshot of live workspace state, exposed through
/// `workspace.snapshot` IPC command. Updated at the end of every frame.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct WorkspaceSnapshot {
    pub focused_group: Option<&'static str>,
    pub focused_pane_id: Option<u64>,
    pub groups: Vec<TabGroupSnapshot>,
    pub workspace_root: String,
    pub shell_short: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TabGroupSnapshot {
    pub id: &'static str,
    pub active_tab_index: usize,
    pub tabs: Vec<TabSnapshot>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TabSnapshot {
    pub pane_id: u64,
    pub title: String,
    /// True when this tab is currently the active member of its group.
    pub is_active: bool,
    /// True when the pane is backed by a live PTY (writable via
    /// `workspace.pane.write`, readable via `workspace.pane.output`).
    pub has_pty: bool,
}

/// In-flight divider drag captured on mouse-down.
#[derive(Debug, Clone)]
struct DragState {
    path: rimeterm_core::layout::SplitPath,
    boundary: usize,
    axis: ratatui::layout::Direction,
    /// Origin coordinates in cells (x for horizontal splits, y for vertical).
    origin_axis_coord: u16,
    /// The parent split's extent along `axis`, used to translate cells → ratio.
    parent_extent: u16,
    /// Ratios at the moment the drag started (undo baseline).
    baseline_ratios: Vec<f32>,
}

/// In-flight agent / external-tool spawn currently booting its PTY. Some
/// coding agents (claude, codex) take multiple seconds to write their
/// first prompt to the pty, and until then the pane looks blank — users
/// reasonably assume "hung". `PendingSpawn` drives a hint-bar spinner
/// that reads `⣷ Initializing Claude Code…  (2.1s)` so it's obvious the
/// tool is starting.
///
/// Cleared when either:
/// - the target pane's grid contains any non-whitespace byte (real first
///   output — the tool responded), or
/// - `PENDING_SPAWN_TIMEOUT` elapses (defensive: if the tool never
///   prints, don't lock the hint bar forever).
#[derive(Debug, Clone)]
pub(crate) struct PendingSpawn {
    pub label: String,
    pub pane_id: PaneId,
    pub started: Instant,
}

/// Deadline after which we stop showing the spinner even if the tool
/// hasn't printed anything. Not a kill switch — the pane keeps running,
/// we just stop nagging the hint bar.
pub(crate) const PENDING_SPAWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Braille dot spinner. One frame per ~100ms of elapsed time so the
/// animation feels alive without being distracting. 8 frames cycles
/// through the standard Unicode block.
pub(crate) const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠇"];

/// Pick a spinner glyph for the given elapsed duration. Pure so tests
/// can lock the cycle shape.
pub(crate) fn spinner_glyph(elapsed: std::time::Duration) -> &'static str {
    let idx = (elapsed.as_millis() / 100) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[idx]
}

/// Decide whether the boot-progress spinner has done its job. Split from
/// `App::expire_pending_spawn` so every branch is unit-testable without
/// spawning a PTY. The classification rules — in order of precedence:
///
/// 1. **Pane vanished** (`grid_sample = None`) → clear. The pane was
///    closed while its child was still booting; nothing to show.
/// 2. **Timeout expired** (`elapsed >= PENDING_SPAWN_TIMEOUT`) → clear.
///    We refuse to nag the hint bar forever if the tool never prints;
///    the pane keeps running, we just stop shouting about it.
/// 3. **First real output present** (any non-whitespace char in the
///    sampled grid) → clear. The tool has responded.
/// 4. Otherwise → keep spinning.
///
/// Rule 3 is why the caller passes the **entire visible viewport**, not
/// just the tail. Alt-screen TUIs (claude, codex, omp) paint their
/// banner at the top and leave the bottom blank; a tail-only sample
/// gave `false` forever and the spinner only cleared on a manual
/// resize (which forces the child to repaint bottom-to-top).
pub(crate) fn pending_spawn_should_clear(
    elapsed: std::time::Duration,
    grid_sample: Option<&str>,
) -> bool {
    let Some(sample) = grid_sample else {
        return true; // pane vanished
    };
    if elapsed >= PENDING_SPAWN_TIMEOUT {
        return true;
    }
    sample.chars().any(|c| !c.is_whitespace())
}

/// Which divider the mouse pointer is currently over. Terminals don't let
/// us swap the OS mouse cursor to a resize icon (no ANSI escape exists),
/// so we compensate visually: the seam paints bright on hover and the hint
/// bar shows `↔ drag to resize`. See `App::hovered_divider`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HoveredDivider {
    pub path: rimeterm_core::layout::SplitPath,
    pub boundary: usize,
    /// Axis of the parent split. `Horizontal` = side-by-side panes,
    /// vertical seam glyph `↔`. `Vertical` = stacked panes, horizontal
    /// seam glyph `↕`.
    pub axis: ratatui::layout::Direction,
    /// The 1-cell-wide rect the seam occupied AT HOVER TIME. Snapshot
    /// only — `draw()` re-lookups the current rect from
    /// `last_dividers` via `(path, boundary)` because ratios can shift
    /// between the Moved event and the next frame (mid-drag,
    /// concurrent keyboard resize, terminal resize). Kept here for
    /// diagnostics and tests.
    pub rect: Rect,
}

/// Which seam a keyboard resize step is aimed at, relative to the focused cell.
#[derive(Copy, Clone, Debug)]
enum ResizeTarget {
    /// The horizontal-axis seam (the vertical divider between left/right columns).
    Horizontal,
    /// The vertical-axis seam (the horizontal divider between the two rows in the column).
    Vertical,
}

/// For a focused group, tell the app which split path + boundary + sign to
/// apply a keyboard resize step to.
///
/// - files : column 0, row 0. `Horizontal` moves the root seam (boundary 0);
///   grow = shift right = +sign. `Vertical` moves the left column seam.
/// - sysmon: column 0, row 1. `Horizontal` = root seam, `Vertical` = left row seam
///   but sign flipped (grow = up = shrink files → so we apply a `-` sign).
/// - agents: column 1, row 0. `Horizontal` = root seam, sign flipped (grow =
///   shift left = shrink files column).
/// - shells: column 1, row 1. Same as agents/sysmon logic.
fn resize_target_for_group(
    gid: rimeterm_core::TabGroupId,
    target: ResizeTarget,
) -> Option<(rimeterm_core::layout::SplitPath, usize, u16, f32)> {
    use rimeterm_core::layout::SplitPath;
    use rimeterm_core::{BUILTIN_AGENTS, BUILTIN_FILES, BUILTIN_SHELLS, BUILTIN_SYSMON};
    match (gid, target) {
        (g, ResizeTarget::Horizontal) if g == BUILTIN_FILES => Some((SplitPath::root(), 0, 0, 1.0)),
        (g, ResizeTarget::Horizontal) if g == BUILTIN_SYSMON => {
            Some((SplitPath::root(), 0, 0, 1.0))
        }
        (g, ResizeTarget::Horizontal) if g == BUILTIN_AGENTS => {
            Some((SplitPath::root(), 0, 0, -1.0))
        }
        (g, ResizeTarget::Horizontal) if g == BUILTIN_SHELLS => {
            Some((SplitPath::root(), 0, 0, -1.0))
        }
        (g, ResizeTarget::Vertical) if g == BUILTIN_FILES => {
            Some((SplitPath::root().push(0), 0, 0, 1.0))
        }
        (g, ResizeTarget::Vertical) if g == BUILTIN_SYSMON => {
            Some((SplitPath::root().push(0), 0, 0, -1.0))
        }
        (g, ResizeTarget::Vertical) if g == BUILTIN_AGENTS => {
            Some((SplitPath::root().push(1), 0, 0, 1.0))
        }
        (g, ResizeTarget::Vertical) if g == BUILTIN_SHELLS => {
            Some((SplitPath::root().push(1), 0, 0, -1.0))
        }
        _ => None,
    }
}

/// Split paths whose ratios should be reset when the user presses `=` while
/// this group is focused.
fn paths_for_group(gid: rimeterm_core::TabGroupId) -> Vec<rimeterm_core::layout::SplitPath> {
    use rimeterm_core::layout::SplitPath;
    use rimeterm_core::{BUILTIN_AGENTS, BUILTIN_FILES, BUILTIN_SHELLS, BUILTIN_SYSMON};
    let column = match gid {
        g if g == BUILTIN_FILES || g == BUILTIN_SYSMON => 0,
        g if g == BUILTIN_AGENTS || g == BUILTIN_SHELLS => 1,
        _ => return Vec::new(),
    };
    vec![SplitPath::root(), SplitPath::root().push(column)]
}

#[allow(dead_code)] // config / event_bus are wired in later milestones
pub struct App {
    workspace_root: PathBuf,
    config: Config,
    shell_choice: ShellChoice,
    shell_short: String,
    menu: AppMenu,
    menu_state: MenuState,
    palette_state: PaletteState,
    picker_state: crate::picker::PickerState,
    commands: std::sync::Arc<CommandRegistry>,
    event_bus: EventBus,
    focus: FocusManager,
    tree: LayoutTree,
    panes: PaneRegistry,
    redraw_tx: mpsc::UnboundedSender<()>,
    /// True while the user is in keyboard Resize mode (§19.12.3). Global keys
    /// are re-routed until Esc/Enter exits.
    resize_mode: bool,
    /// Snapshot of live state exposed to IPC commands. Updated at the end of
    /// every frame; commands see a value that's at most one tick stale.
    snapshot: Arc<parking_lot::RwLock<WorkspaceSnapshot>>,
    /// Live PTY sessions keyed by pane id. Cloneable so IPC handlers can
    /// share write access without holding App mutably.
    session_writes:
        Arc<parking_lot::Mutex<std::collections::HashMap<PaneId, rimeterm_pty::Session>>>,
    redraw_rx: mpsc::UnboundedReceiver<()>,
    flags: Arc<ActionFlags>,
    should_quit: bool,
    /// Transient status-bar hint (e.g. "Ctrl+T rejected: files is fixed").
    hint: Option<(String, Instant)>,
    /// Last computed pane area (rect passed to LayoutTree in the most recent
    /// draw). Needed by mouse events so hit-tests use current-frame geometry.
    last_pane_area: Rect,
    /// Divider list matching `last_pane_area`. Cached at frame end.
    last_dividers: Vec<rimeterm_core::layout::Divider>,
    /// Cached tab-strip hit rects per group, populated during `draw`. Used
    /// by `on_mouse` to route clicks on tab titles / `[+]` back into the
    /// same commands the keyboard uses. Fresh every frame.
    last_tab_strips: Vec<(
        rimeterm_core::tabs::TabGroupId,
        crate::tab_strip::TabStripHits,
    )>,
    /// Cached per-pane outer rect (strip-stripped, i.e. the actual rect
    /// `pane.render` received). Different from `LayoutTree::compute_rects`
    /// output, which returns the full quadrant cell including its tab strip.
    last_pane_outer_rects: Vec<(PaneId, Rect)>,
    /// Divider under the mouse cursor RIGHT NOW (updated on every
    /// MouseEventKind::Moved). Painted with a hover highlight and shows a
    /// `↔ drag to resize` hint in the bottom bar so users know the seam
    /// is interactive — terminal apps can't change the OS mouse-cursor
    /// shape, so we compensate visually. Cleared to `None` when the
    /// pointer leaves any divider rect.
    ///
    /// Keyed by `(SplitPath, boundary)` — the same key that identifies
    /// a divider in `last_dividers`. `Direction` is cached so the
    /// hint / glyph don't need a second lookup.
    hovered_divider: Option<HoveredDivider>,
    /// Populated the moment an agent / external-tool spawn is queued
    /// (via `PaneMutation::OpenAgent`). Drives a hint-bar spinner
    /// (`⣷ Initializing …`) so the user knows the terminal isn't
    /// hung while claude/codex/etc take seconds to boot their PTY.
    /// Cleared when the target pane produces first output or after
    /// `PENDING_SPAWN_TIMEOUT`.
    pending_spawn: Option<PendingSpawn>,
    /// Reverse map from agent PaneId → static registry id (`omp` / `codex`
    /// / `claude` / `pi`). Populated on spawn, consumed by
    /// `persist_agents_state` to write the on-disk file.
    pane_agent_id: std::collections::HashMap<PaneId, &'static str>,
    /// In-progress divider drag. `None` when idle.
    active_drag: Option<DragState>,
    /// Snapshot of default ratios so we can `= / 0` reset.
    default_ratios: Vec<(rimeterm_core::layout::SplitPath, Vec<f32>)>,
    /// Queue of `PaneMutation`s pushed by IPC handlers, drained on each
    /// tick of the main loop. Wrapped in `parking_lot::Mutex` — never held
    /// across an await point.
    pending_mutations: Arc<parking_lot::Mutex<std::collections::VecDeque<PaneMutation>>>,
}

impl App {
    pub fn new(workspace_root: PathBuf, config: Config) -> Result<Self> {
        let shell_choice = pick_shell(&config)?;
        let shell_short: String = shell_choice.short_name().into();
        info!(
            shell = shell_short.as_str(),
            path = %shell_choice.path().unwrap().display(),
            "shell selected"
        );

        let (redraw_tx, redraw_rx) = mpsc::unbounded_channel();
        let session_writes: Arc<
            parking_lot::Mutex<std::collections::HashMap<PaneId, rimeterm_pty::Session>>,
        > = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));

        // Everything except the shells group is a Placeholder until later
        // milestones bring in the real PTY / native providers.
        let mut panes = PaneRegistry::new();

        let mut files_members = Vec::new();
        for spec in &config.files.tabs {
            let icon = match spec.id.as_str() {
                "gitui" => "🌿",
                _ => "📁",
            };
            let color = match spec.id.as_str() {
                "gitui" => Color::Green,
                _ => Color::Cyan,
            };
            let id = build_external_pane(
                &mut panes,
                &session_writes,
                spec,
                &workspace_root,
                redraw_tx.clone(),
                icon,
                color,
                "files",
            )?;
            files_members.push(id);
        }
        let mut sysmon_members = Vec::new();
        for spec in &config.sysmon.tabs {
            let icon = match spec.id.as_str() {
                "trippy" => "🛰",
                _ => "📊",
            };
            let color = match spec.id.as_str() {
                "trippy" => Color::Blue,
                _ => Color::Magenta,
            };
            let id = build_external_pane(
                &mut panes,
                &session_writes,
                spec,
                &workspace_root,
                redraw_tx.clone(),
                icon,
                color,
                "sysmon",
            )?;
            sysmon_members.push(id);
        }

        let mut agents_members = Vec::new();
        // (pane_id, static registry id) for each agent tab we spawn during
        // startup — either from config or from the persisted state file.
        // Handed into App::pane_agent_id below so `persist_agents_state`
        // can rebuild the on-disk list correctly across restarts.
        let mut startup_agent_ids: Vec<(PaneId, &'static str)> = Vec::new();
        for spec in &config.agents.tabs {
            let id = build_agent_pane(
                &mut panes,
                &session_writes,
                spec,
                &workspace_root,
                redraw_tx.clone(),
            )?;
            agents_members.push(id);
            // Try to map the config spec id back to a registry entry so
            // we can persist it. Config-only specs (rare) get skipped.
            if let Some(reg) = rimeterm_pty::agent_registry::find(&spec.id) {
                startup_agent_ids.push((id, reg.id));
            }
        }

        // Persisted picks from previous sessions (see §14 / C-current).
        // Each id is looked up in AGENT_REGISTRY; unknown / renamed ids
        // are skipped silently rather than crashing the workspace.
        if agents_members.is_empty() {
            if let Some(state_path) =
                rimeterm_config::agents_state::workspace_state_file(&workspace_root)
            {
                match rimeterm_config::agents_state::AgentsState::load_or_default(&state_path) {
                    Ok(state) => {
                        for id in &state.tabs {
                            let Some(spec) = rimeterm_pty::agent_registry::find(id) else {
                                tracing::warn!(
                                    agent_id = id.as_str(),
                                    "persisted agent id no longer in registry — skipping"
                                );
                                continue;
                            };
                            let ext_spec = rimeterm_config::AgentSpec {
                                id: spec.id.to_string(),
                                label: spec.label.to_string(),
                                command: spec.argv.iter().map(|s| s.to_string()).collect(),
                                install_hint: Some(spec.install_hint.to_string()),
                            };
                            match build_agent_pane(
                                &mut panes,
                                &session_writes,
                                &ext_spec,
                                &workspace_root,
                                redraw_tx.clone(),
                            ) {
                                Ok(pane_id) => {
                                    agents_members.push(pane_id);
                                    startup_agent_ids.push((pane_id, spec.id));
                                }
                                Err(e) => {
                                    tracing::warn!(agent_id = id.as_str(), error = %e, "failed to restore persisted agent tab")
                                }
                            }
                        }
                        if !agents_members.is_empty() {
                            tracing::info!(
                                path = %state_path.display(),
                                count = agents_members.len(),
                                "restored persisted agent tabs"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path = %state_path.display(), error = %e, "failed to load agents state")
                    }
                }
            }
        }

        // §14 C14: agents group starts with a picker placeholder when
        // no config tabs and no persisted state produced a real member.
        // TabGroup::new asserts non-empty on construction; the
        // placeholder is auto-closed the first time a real agent lands
        // (see `new_agent_tab_in`).
        if agents_members.is_empty() {
            let hint = format_agent_picker_hint();
            let picker = PlaceholderPane::new(AGENT_PICKER_TITLE, hint, "🤖", Color::LightMagenta);
            let id = picker.id();
            panes.insert(Box::new(picker));
            agents_members.push(id);
        }

        // shells starts with a real PTY.
        let first = spawn_shell(
            &shell_choice,
            workspace_root.clone(),
            "shell-1".into(),
            80,
            24,
            redraw_tx.clone(),
        )?;
        let first_id = first.pane.id();
        session_writes
            .lock()
            .insert(first_id, first.pane.session().clone());
        panes.insert(Box::new(first.pane));

        // Groups.
        let files = TabGroup::new(
            BUILTIN_FILES,
            files_members,
            MembersPolicy::Fixed,
            PaneKind::Files,
        );
        let sysmon = TabGroup::new(
            BUILTIN_SYSMON,
            sysmon_members,
            MembersPolicy::Fixed,
            PaneKind::Sysmon,
        );
        let agents = TabGroup::new(
            BUILTIN_AGENTS,
            agents_members,
            MembersPolicy::Open { max: 16 },
            PaneKind::AgentChat,
        );
        let shells = TabGroup::new(
            BUILTIN_SHELLS,
            vec![first_id],
            MembersPolicy::Open { max: 16 },
            PaneKind::Shell,
        );

        // Layout: horizontal split (left | right), each column a vertical split
        // of two tab groups. Ratios match §19.2 closed state (0.35 / 0.65 cols,
        // 0.65 / 0.35 rows on the left, 0.55 / 0.45 rows on the right).
        let root = LayoutNode::split(
            Direction::Horizontal,
            vec![0.35, 0.65],
            vec![
                LayoutNode::split(
                    Direction::Vertical,
                    vec![0.65, 0.35],
                    vec![LayoutNode::tabs(files), LayoutNode::tabs(sysmon)],
                ),
                LayoutNode::split(
                    Direction::Vertical,
                    vec![0.55, 0.45],
                    vec![LayoutNode::tabs(agents), LayoutNode::tabs(shells)],
                ),
            ],
        );
        let mut tree = LayoutTree::new(root).map_err(|e| anyhow!("layout tree: {e}"))?;
        let default_ratios = snapshot_all_ratios(&tree);

        // Restore any previously persisted ratios for this workspace.
        if let Some(state_path) =
            rimeterm_config::layout_state::workspace_state_file(&workspace_root)
        {
            match rimeterm_config::layout_state::LayoutState::load_or_default(&state_path) {
                Ok(state) if !state.is_empty() => {
                    apply_persisted_state(&mut tree, &state);
                    info!(
                        path = %state_path.display(),
                        splits = state.splits.len(),
                        "restored persisted layout state",
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "failed to load layout state; using defaults");
                }
            }
        }

        let event_bus = EventBus::default();
        let mut focus = FocusManager::new(event_bus.clone());
        focus.set_focus(first_id, Some(BUILTIN_SHELLS));

        let flags = Arc::new(ActionFlags::default());
        let snapshot = Arc::new(parking_lot::RwLock::new(WorkspaceSnapshot::default()));
        let pending_mutations: Arc<parking_lot::Mutex<std::collections::VecDeque<PaneMutation>>> =
            Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::new()));
        let mut commands = CommandRegistry::new();
        register_commands(
            &mut commands,
            Arc::clone(&flags),
            Arc::clone(&snapshot),
            Arc::clone(&session_writes),
            Arc::clone(&pending_mutations),
            redraw_tx.clone(),
        )?;

        Ok(Self {
            workspace_root,
            config,
            shell_choice,
            shell_short,
            menu: AppMenu::v0_1_default(),
            menu_state: MenuState::default(),
            palette_state: PaletteState::default(),
            picker_state: crate::picker::PickerState::default(),
            commands: std::sync::Arc::new(commands),
            event_bus,
            focus,
            tree,
            panes,
            redraw_tx,
            redraw_rx,
            flags,
            should_quit: false,
            hint: None,
            resize_mode: false,
            snapshot,
            session_writes,
            last_pane_area: Rect::default(),
            last_dividers: Vec::new(),
            last_tab_strips: Vec::new(),
            last_pane_outer_rects: Vec::new(),
            hovered_divider: None,
            pending_spawn: None,
            pane_agent_id: startup_agent_ids.into_iter().collect(),
            active_drag: None,
            default_ratios,
            pending_mutations,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter().context("enter alt-screen / raw mode")?;
        let mut input = EventStream::new();

        // Spawn the local IPC server (§11). Shuts down when this handle
        // is dropped at the end of `run`.
        let ipc_shutdown = self.spawn_ipc_server().await;

        guard.terminal.draw(|f| {
            let cursor = self.draw(f.area(), f.buffer_mut());
            if let Some((x, y)) = cursor {
                f.set_cursor_position((x, y));
            }
        })?;

        loop {
            if self.should_quit || self.flags.quit.load(Ordering::Relaxed) {
                self.shutdown();
                break;
            }
            self.drain_mutations();
            self.drain_flags();
            self.expire_hint();
            self.expire_pending_spawn();

            tokio::select! {
                Some(evt) = input.next() => {
                    match evt {
                        Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => self.on_key(k),
                        Ok(Event::Mouse(m)) => self.on_mouse(m),
                        Ok(Event::Resize(_, _)) => {}
                        _ => {}
                    }
                }
                Some(_) = self.redraw_rx.recv() => {
                    while self.redraw_rx.try_recv().is_ok() {}
                }
                _ = tokio::time::sleep(Duration::from_millis(16)) => {}
            }

            guard.terminal.draw(|f| {
                let cursor = self.draw(f.area(), f.buffer_mut());
                if let Some((x, y)) = cursor {
                    f.set_cursor_position((x, y));
                }
            })?;
        }
        if let Some(tx) = ipc_shutdown {
            let _ = tx.send(()).await;
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        if self.menu_state.open {
            match menu_key(&mut self.menu_state, &self.menu, key) {
                MenuKeyOutcome::Run(cmd) => {
                    if let Err(e) = self.commands.run(cmd) {
                        warn!(command = cmd, error = %e, "menu command failed");
                    }
                }
                _ => {}
            }
            return;
        }

        if self.picker_state.open {
            match crate::picker::handle_key(&mut self.picker_state, key) {
                crate::picker::PickerOutcome::Run(action) => {
                    self.run_picker_action(action);
                }
                _ => {}
            }
            return;
        }

        if self.palette_state.open {
            let entries = self.command_entries();
            match palette_key(&mut self.palette_state, &entries, key) {
                PaletteOutcome::Run(cmd) => {
                    if let Err(e) = self.commands.run(cmd) {
                        warn!(command = cmd, error = %e, "palette command failed");
                    }
                }
                _ => {}
            }
            return;
        }

        if self.resize_mode {
            self.on_resize_key(key);
            return;
        }

        match Keymap::dispatch(key) {
            KeymapOutcome::Run(cmd) => {
                if let Err(e) = self.commands.run(cmd) {
                    warn!(command = cmd, error = %e, "global command failed");
                }
                return;
            }
            KeymapOutcome::Consumed => return,
            KeymapOutcome::Passthrough => {}
        }

        if let Some(id) = self.focus.focused_pane() {
            // Placeholder pane [I] shortcut: if the focused pane advertises
            // an install command (via PaneProvider::install_command) and
            // the user pressed a plain `i` / `I`, open a fresh shell tab
            // with the command pre-typed for them to review + Enter.
            //
            // Skip modifiers (Ctrl+I, Alt+I, …) so a script binding those
            // to something else still works. `KeyModifiers::SHIFT` for `I`
            // is inherent to that keycode on some terminals — accept both.
            use crossterm::event::{KeyCode, KeyModifiers};
            let plain_i = matches!(key.code, KeyCode::Char('i') | KeyCode::Char('I'))
                && (key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT);
            if plain_i {
                if let Some(pane) = self.panes.get(id) {
                    if let Some(cmd) = pane.install_command() {
                        let cmd = cmd.to_string();
                        self.set_hint(format!("⚙ opening install shell: {}", cmd));
                        self.pending_mutations
                            .lock()
                            .push_back(PaneMutation::OpenShellAndType { command: cmd });
                        let _ = self.redraw_tx.send(());
                        return;
                    }
                }
            }
            if let Some(pane) = self.panes.get_mut(id) {
                let _ = pane.on_key(key);
            }
        }
    }

    /// Handle a key while in Resize mode (§19.12.3).
    ///
    /// - Esc / Enter: exit resize mode.
    /// - `=`         : restore this cell's parent-split to its default ratios.
    /// - `0`         : restore *every* split's ratios.
    /// - H / L       : shrink / grow along the horizontal seam adjacent to the focused cell.
    /// - K / J       : shrink / grow along the vertical seam adjacent to the focused cell.
    /// - Shift+HJKL  : step of 5 cells instead of 1.
    fn on_resize_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.resize_mode = false;
                self.set_hint("Resize mode: off".into());
                return;
            }
            KeyCode::Char('=') => {
                self.reset_focused_split();
                return;
            }
            KeyCode::Char('0') => {
                self.reset_all_splits();
                return;
            }
            _ => {}
        }
        let big = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT);
        let step_cells: i32 = if big { 5 } else { 1 };
        let (target, sign) = match key.code {
            KeyCode::Char('h') | KeyCode::Char('H') => (ResizeTarget::Horizontal, -1),
            KeyCode::Char('l') | KeyCode::Char('L') => (ResizeTarget::Horizontal, 1),
            KeyCode::Char('k') | KeyCode::Char('K') => (ResizeTarget::Vertical, -1),
            KeyCode::Char('j') | KeyCode::Char('J') => (ResizeTarget::Vertical, 1),
            _ => return,
        };
        self.resize_step(target, sign * step_cells);
    }

    fn resize_step(&mut self, target: ResizeTarget, cells: i32) {
        let Some(gid) = self.focus.focused_group() else {
            return;
        };
        let Some((path, boundary, parent_extent, adjust_sign)) =
            resize_target_for_group(gid, target)
        else {
            return;
        };
        // Convert cells → ratio in the *current* parent extent. We resolve the
        // parent split's rect using the cached pane area.
        let parent_rect = split_parent_rect(&self.tree, self.last_pane_area, &path);
        let extent = parent_rect
            .map(|r| match target {
                ResizeTarget::Horizontal => r.width,
                ResizeTarget::Vertical => r.height,
            })
            .unwrap_or(parent_extent);
        if extent == 0 {
            return;
        }
        let delta_ratio = adjust_sign * cells as f32 / extent as f32;
        let floors = min_size_floors(&self.tree, &path, extent);
        match self
            .tree
            .adjust_ratio(&path, boundary, delta_ratio, &floors)
        {
            Ok(()) => {}
            Err(rimeterm_core::layout::RatioError::BelowMinSize) => {
                self.set_hint("⛔ at minimum size".into());
            }
            Err(_) => {}
        }
    }

    fn reset_focused_split(&mut self) {
        let Some(gid) = self.focus.focused_group() else {
            return;
        };
        // Restore both the group's column split and its row split — this
        // matches the user mental model of "reset this cell's neighborhood".
        let paths = paths_for_group(gid);
        for path in paths {
            if let Some(defaults) = self
                .default_ratios
                .iter()
                .find(|(p, _)| p == &path)
                .map(|(_, r)| r.clone())
            {
                let _ = self.tree.set_ratios(&path, defaults);
            }
        }
        self.set_hint("cell reset to defaults".into());
    }

    fn reset_all_splits(&mut self) {
        for (path, ratios) in self.default_ratios.clone() {
            let _ = self.tree.set_ratios(&path, ratios);
        }
        self.set_hint("all splits reset to defaults".into());
    }

    /// Handle a mouse event (§19.12: draggable dividers).
    ///
    /// Route a mouse event. Priority:
    /// 1. Active divider drag (from a prior Down on a seam) — resize the layout.
    /// 2. Down / Up / Drag / Scroll on a pane rect — forward to that pane's
    ///    `PaneProvider::on_mouse` (PtyPane translates to SGR mouse
    ///    sequences and writes into the child's stdin, so yazi / omp get
    ///    click, scroll, drag-select natively).
    /// 3. Down on a bare divider (no pane hit) — start a drag.
    /// 4. Down on empty space (unlikely) — no-op.
    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        // --- Active drag takes precedence ---
        if let MouseEventKind::Drag(MouseButton::Left) = m.kind {
            if self.active_drag.is_some() {
                self.mouse_drag(m.column, m.row);
                return;
            }
        }
        if let MouseEventKind::Up(MouseButton::Left) = m.kind {
            if self.active_drag.take().is_some() {
                // §19.12.6: on mouse-up the throttler is bypassed so the
                // final drag size lands exactly on the PTY.
                self.flush_pending_resizes();
                // Clear the hover cache: the seam almost certainly moved
                // under the cursor during the drag, so any pre-drag
                // `hovered_divider` is stale. The very next `Moved`
                // event will re-populate it with the current position.
                if self.hovered_divider.take().is_some() {
                    let _ = self.redraw_tx.send(());
                }
                return;
            }
        }

        // --- Move (no button): hover tracking for dividers ---
        //
        // Terminals don't expose a hook to change the OS mouse cursor
        // shape (no ANSI escape covers it), so we mark the seam itself
        // as interactive: paint it bright and drop `↔ drag to resize`
        // into the hint bar. Compare-then-set avoids gratuitous
        // redraws when the pointer slides along the same divider row.
        if let MouseEventKind::Moved = m.kind {
            let new_hover = find_hovered_divider(&self.last_dividers, m.column, m.row);
            if new_hover != self.hovered_divider {
                self.hovered_divider = new_hover;
                // Wake the main loop so the seam repaints in the next
                // frame instead of waiting for the next input event.
                let _ = self.redraw_tx.send(());
            }
            return;
        }

        // --- Right Down: open the context menu (never forward to child) ---
        if let MouseEventKind::Down(MouseButton::Right) = m.kind {
            self.open_context_menu(m.column, m.row);
            return;
        }

        // --- Left Down: dispatch by zone (divider → tab strip → pane) ---
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            // 1. Divider drag.
            if let Some(d) = self
                .last_dividers
                .iter()
                .find(|d| point_in_rect(m.column, m.row, d.visual.rect))
                .cloned()
            {
                self.start_divider_drag(d, m.column, m.row);
                return;
            }
            // 2. Tab strip: activate / close / fire the group's `[+]`.
            if let Some(hit) = self.tab_hit(m.column, m.row) {
                match hit {
                    TabStripHit::Activate { gid, idx } => self.activate_tab(gid, idx),
                    TabStripHit::Close { gid, idx } => self.close_tab_at(gid, idx),
                    TabStripHit::Plus { gid } => self.new_tab_in(gid),
                }
                return;
            }
            // 3. Fell into a pane rect: focus + forward the Down.
            self.focus_pane_at(m.column, m.row);
        }

        // --- Drag / Up / Scroll on a pane rect: forward as SGR to child ---
        let Some((pane_id, outer_rect)) = self.pane_outer_at(m.column, m.row) else {
            return;
        };
        if let Some(pane) = self.panes.get_mut(pane_id) {
            let _ = pane.on_mouse(m, outer_rect);
        }
    }

    /// Start a divider drag from an already-hit `d`. Called by `on_mouse`
    /// after it has confirmed the click lands on a seam.
    fn start_divider_drag(&mut self, d: rimeterm_core::layout::Divider, col: u16, row: u16) {
        let Some(parent_rect) = split_parent_rect(&self.tree, self.last_pane_area, &d.path) else {
            return;
        };
        let axis = d.visual.axis;
        let (origin, extent) = match axis {
            Direction::Horizontal => (col, parent_rect.width),
            Direction::Vertical => (row, parent_rect.height),
        };
        let baseline_ratios = self.tree.ratios_at(&d.path).unwrap_or_default();
        self.active_drag = Some(DragState {
            path: d.path,
            boundary: d.boundary,
            axis,
            origin_axis_coord: origin,
            parent_extent: extent,
            baseline_ratios,
        });
    }

    /// Focus the pane under `(col, row)` and activate its tab within the
    /// owning group. Silent no-op if the click missed every pane. Uses
    /// the strip-stripped `last_pane_outer_rects` cache — so a click on
    /// the tab strip is NOT treated as a pane click (the tab hit path
    /// in `on_mouse` runs first).
    fn focus_pane_at(&mut self, col: u16, row: u16) {
        let Some((pane_id, _)) = self.pane_outer_at(col, row) else {
            return;
        };
        let owner = self.tree.tab_groups().iter().find_map(|g| {
            g.members()
                .iter()
                .position(|m| *m == pane_id)
                .map(|idx| (g.id(), idx))
        });
        if let Some((gid, idx)) = owner {
            if let Some(group) = self.tree.find_tab_group_mut(gid) {
                let _ = group.goto(idx);
            }
            self.focus.set_focus(pane_id, Some(gid));
        }
        // Convenience: clicking the "Pick an agent" placeholder pane also
        // opens the picker so users don't have to hunt for the `[+]`.
        // Cheap — one HashMap lookup + a string compare.
        if let Some(pane) = self.panes.get(pane_id) {
            if pane.title() == AGENT_PICKER_TITLE {
                self.open_agent_picker();
            }
        }
    }

    /// Reverse-lookup: which pane sits under `(col, row)` in the last-drawn
    /// pane area? Uses the per-frame `last_pane_outer_rects` cache built by
    /// `draw`, which stores the actual `pane_rect` each provider was handed
    /// (i.e. the quadrant cell **minus** its 1-row tab strip). Returns the
    /// pane id together with that outer rect so callers can forward the
    /// event without recomputing geometry.
    fn pane_outer_at(&self, col: u16, row: u16) -> Option<(PaneId, Rect)> {
        self.last_pane_outer_rects
            .iter()
            .find(|(_, r)| point_in_rect(col, row, *r))
            .copied()
    }

    /// What the user meant by clicking somewhere on a cached tab strip.
    /// `Activate` = switch to that tab. `Close` = close that tab (same
    /// semantics as `workspace.pane.close`). `Plus` = new-tab dispatch.
    fn tab_hit(&self, col: u16, row: u16) -> Option<TabStripHit> {
        for (gid, hits) in &self.last_tab_strips {
            if !point_in_rect(col, row, hits.rect) {
                continue;
            }
            // Close hits sit inside the tab rect (last cell of the label
            // area) — check them BEFORE the activate rect so clicking `×`
            // doesn't also switch to that tab.
            for (idx, r) in &hits.closes {
                if point_in_rect(col, row, *r) {
                    return Some(TabStripHit::Close {
                        gid: *gid,
                        idx: *idx,
                    });
                }
            }
            for (idx, r) in &hits.tabs {
                if point_in_rect(col, row, *r) {
                    return Some(TabStripHit::Activate {
                        gid: *gid,
                        idx: *idx,
                    });
                }
            }
            if let Some(plus) = hits.plus {
                if point_in_rect(col, row, plus) {
                    return Some(TabStripHit::Plus { gid: *gid });
                }
            }
        }
        None
    }

    /// Close whichever tab the user clicked `×` on. Uses the same policy
    /// path as `workspace.pane.close` (delegates to `close_pane_by_id`),
    /// so Open-group last-member protection kicks in identically.
    fn close_tab_at(&mut self, gid: TabGroupId, idx: usize) {
        let Some(pane_id) = self
            .tree
            .find_tab_group(gid)
            .and_then(|g| g.members().get(idx).copied())
        else {
            return;
        };
        if let Err(e) = self.close_pane_by_id(pane_id) {
            self.set_hint(format!("⛔ {}", e));
        }
    }

    /// Activate tab `idx` inside `gid` and move keyboard focus to its
    /// active pane. Silent no-op if the group or index is stale (racing
    /// with a `pane.close` from IPC, say).
    fn activate_tab(&mut self, gid: TabGroupId, idx: usize) {
        let pane_id = match self.tree.find_tab_group_mut(gid) {
            Some(group) => {
                if group.goto(idx).is_err() {
                    return;
                }
                group.active_pane()
            }
            None => return,
        };
        if let Some(pane_id) = pane_id {
            self.focus.set_focus(pane_id, Some(gid));
        }
    }

    /// Dispatch the `[+]` affordance for `gid`. shells → spawn a new shell
    /// immediately (there's only one choice). agents → open the picker
    /// modal populated with `AGENT_REGISTRY` entries; each row fires the
    /// corresponding `agents.pick.<id>` command on Enter.
    fn new_tab_in(&mut self, gid: TabGroupId) {
        if gid == BUILTIN_SHELLS {
            if let Err(e) = self.new_shell_tab_in(gid) {
                self.set_hint(format!("⛔ {}", e));
            }
        } else if gid == BUILTIN_AGENTS {
            self.open_agent_picker();
        }
        // Fixed groups (files/sysmon) have no plus rect in the first place.
    }

    /// Populate and open the agent picker. Each entry maps to one of the
    /// four `agents.pick.<id>` commands registered at startup. Rows for
    /// agents whose binary isn't on PATH are shown greyed out with a
    /// `(not installed)` note so the user can still see the full menu.
    fn open_agent_picker(&mut self) {
        // AGENT_REGISTRY is already alphabetical (see the
        // `registry_labels_are_case_insensitively_alphabetical` test);
        // walk it in order so the picker mirrors the registry.
        //
        // For each detected row:
        // - installed → intent = `agents.pick.<id>` (dispatched by
        //   `run_context_intent`, which runs the same command palette
        //   entry registered in `register_commands`)
        // - missing → disabled row with `(not installed)` note so the
        //   user still sees the option and its install hint via the
        //   placeholder pane / palette description
        //
        // Using `Intent` (not `Command`) lets us keep the CommandId as
        // `&'static str` while still binding to a runtime-shaped id;
        // the intent string carries the agent id, the arm below looks
        // it up in the registry.
        let entries: Vec<crate::picker::PickerEntry> = rimeterm_pty::agent_registry::detect_all()
            .into_iter()
            .map(|a| {
                if a.is_available() {
                    crate::picker::PickerEntry::intent(a.label, format!("agents.pick.{}", a.id))
                } else {
                    crate::picker::PickerEntry::disabled(a.label, "(not installed)")
                }
            })
            .collect();
        self.picker_state.open_with(AGENT_PICKER_TITLE, entries);
    }

    /// Dispatch whatever the picker just returned. Command actions go
    /// through the CommandRegistry; Intent actions carry a string tag we
    /// parse here (`tab.close:<gid>:<idx>`, `tab.activate:<gid>:<idx>`,
    /// `pane.new_shell`, `pane.open_agent_picker`, …) — kept as strings
    /// so the picker module doesn't need to know about App types.
    fn run_picker_action(&mut self, action: crate::picker::PickerAction) {
        match action {
            crate::picker::PickerAction::Command(cmd) => {
                if let Err(e) = self.commands.run(cmd) {
                    warn!(command = cmd, error = %e, "picker command failed");
                }
            }
            crate::picker::PickerAction::Intent(intent) => {
                self.run_context_intent(&intent);
            }
            crate::picker::PickerAction::Disabled => {}
        }
    }

    /// Parse and dispatch a context-menu intent string. Format:
    ///   `tab.activate:<group>:<idx>`
    ///   `tab.close:<group>:<idx>`
    ///   `agents.pick`
    ///   `shells.new`
    ///   `pane.focus:<pane_id>`
    ///   `resize.toggle`
    ///   `layout.reset`
    fn run_context_intent(&mut self, intent: &str) {
        // Picker rows for the agents quadrant emit
        // `agents.pick.<agent_id>` intents (see `open_agent_picker`).
        // We DO NOT dispatch through `commands.run("agents.pick.<id>")`
        // here: that closure calls `ack_rx.recv_timeout(5s)` which
        // would deadlock because we ARE the main loop — nothing else
        // can drain the mutation while this thread is blocked. Instead
        // we perform the spawn inline (same code the drain would take)
        // so the user sees the "Initializing …" spinner immediately.
        //
        // The registered `agents.pick.<id>` Command survives for IPC
        // consumers (rimectl / MCP), where the closure runs on a
        // spawn_blocking task and the ack channel works fine.
        if let Some(agent_id) = intent.strip_prefix("agents.pick.") {
            let spec_opt = rimeterm_pty::agent_registry::AGENT_REGISTRY
                .iter()
                .find(|s| s.id == agent_id);
            match spec_opt {
                Some(spec) => {
                    let label = spec.label;
                    match self.new_agent_tab_in(BUILTIN_AGENTS, spec) {
                        Ok(pane_id) => {
                            // Same spinner setup the drain arm uses —
                            // hint bar shows `⣷ Initializing X…` until
                            // the tool prints its first byte.
                            self.pending_spawn = Some(PendingSpawn {
                                label: label.to_string(),
                                pane_id,
                                started: Instant::now(),
                            });
                            // Kick the loop so the spinner paints on
                            // the very next frame instead of waiting
                            // for the next input event.
                            let _ = self.redraw_tx.send(());
                        }
                        Err(e) => {
                            self.set_hint(format!("⛔ open {}: {}", label, e));
                        }
                    }
                }
                None => {
                    self.set_hint(format!("⛔ unknown agent id `{}`", agent_id));
                }
            }
            return;
        }
        let mut parts = intent.split(':');
        match parts.next() {
            Some("tab.activate") => {
                if let (Some(gid), Some(idx)) = (
                    parts.next().and_then(parse_group_id),
                    parts.next().and_then(|s| s.parse::<usize>().ok()),
                ) {
                    self.activate_tab(gid, idx);
                }
            }
            Some("tab.close") => {
                if let (Some(gid), Some(idx)) = (
                    parts.next().and_then(parse_group_id),
                    parts.next().and_then(|s| s.parse::<usize>().ok()),
                ) {
                    self.close_tab_at(gid, idx);
                }
            }
            Some("agents.pick") => self.open_agent_picker(),
            Some("shells.new") => {
                if let Err(e) = self.new_shell_tab_in(BUILTIN_SHELLS) {
                    self.set_hint(format!("⛔ {}", e));
                }
            }
            Some("pane.focus") => {
                if let Some(pane_id) = parts.next().and_then(|s| s.parse::<u64>().ok()).map(PaneId)
                {
                    let owner = self.tree.tab_groups().iter().find_map(|g| {
                        g.members()
                            .iter()
                            .position(|m| *m == pane_id)
                            .map(|idx| (g.id(), idx))
                    });
                    if let Some((gid, idx)) = owner {
                        if let Some(group) = self.tree.find_tab_group_mut(gid) {
                            let _ = group.goto(idx);
                        }
                        self.focus.set_focus(pane_id, Some(gid));
                    }
                }
            }
            Some("resize.toggle") => {
                self.resize_mode = !self.resize_mode;
                let msg = if self.resize_mode {
                    "Resize mode: H/L/K/J adjust · Shift = ×5 · = restore · Esc/Enter exit"
                } else {
                    "Resize mode: off"
                };
                self.set_hint(msg.into());
            }
            Some("layout.reset") => self.reset_layout(),
            _ => {}
        }
    }

    /// Build a context menu for the cell at `(col, row)` and open it as a
    /// picker. Only fired by right-click.
    fn open_context_menu(&mut self, col: u16, row: u16) {
        let mut entries: Vec<crate::picker::PickerEntry> = Vec::new();

        // Divider hit — anchor inside the full pane area so the popup
        // can appear on either side of the seam without spilling into
        // the tab strip or hint bar.
        if self
            .last_dividers
            .iter()
            .any(|d| point_in_rect(col, row, d.visual.rect))
        {
            entries.push(crate::picker::PickerEntry::intent(
                "Toggle Resize mode",
                "resize.toggle",
            ));
            entries.push(crate::picker::PickerEntry::intent(
                "Reset splits to defaults",
                "layout.reset",
            ));
            let anchor = crate::picker::PickerAnchor::Anchored {
                x: col,
                y: row,
                bounds: self.last_pane_area,
            };
            self.picker_state
                .open_with_anchor("Divider", entries, anchor);
            return;
        }

        // Tab strip hit — anchor inside the owning pane's outer rect so
        // the menu appears attached to that group's cell (not floating
        // over a neighbour).
        if let Some(hit) = self.tab_hit(col, row) {
            let (gid, is_plus_only) = match hit {
                TabStripHit::Activate { gid, idx } | TabStripHit::Close { gid, idx } => {
                    let is_open = matches!(
                        self.tree.find_tab_group(gid).map(|g| g.policy()),
                        Some(rimeterm_core::tabs::MembersPolicy::Open { .. })
                    );
                    entries.push(crate::picker::PickerEntry::intent(
                        "Activate this tab",
                        format!("tab.activate:{}:{}", gid, idx),
                    ));
                    if is_open {
                        entries.push(crate::picker::PickerEntry::intent(
                            "Close this tab",
                            format!("tab.close:{}:{}", gid, idx),
                        ));
                    } else {
                        entries.push(crate::picker::PickerEntry::disabled(
                            "Close this tab",
                            "(fixed group)",
                        ));
                    }
                    push_group_new_entry(&mut entries, gid);
                    (gid, false)
                }
                TabStripHit::Plus { gid } => {
                    push_group_new_entry(&mut entries, gid);
                    (gid, true)
                }
            };
            let bounds = self.group_bounds(gid).unwrap_or(self.last_pane_area);
            let title = if is_plus_only {
                format!("Group · {}", gid)
            } else {
                format!("Tab · {}", gid)
            };
            let anchor = crate::picker::PickerAnchor::Anchored {
                x: col,
                y: row,
                bounds,
            };
            self.picker_state.open_with_anchor(title, entries, anchor);
            return;
        }

        // Pane hit — anchor inside that pane's outer rect so the menu
        // stays on the shell / yazi / whatever the user right-clicked.
        if let Some((pane_id, outer_rect)) = self.pane_outer_at(col, row) {
            let owner = self.tree.tab_groups().iter().find_map(|g| {
                g.members()
                    .iter()
                    .position(|m| *m == pane_id)
                    .map(|i| (g.id(), i, g.policy()))
            });
            entries.push(crate::picker::PickerEntry::intent(
                "Focus this pane",
                format!("pane.focus:{}", pane_id.0),
            ));
            if let Some((gid, idx, policy)) = owner {
                if matches!(policy, rimeterm_core::tabs::MembersPolicy::Open { .. }) {
                    entries.push(crate::picker::PickerEntry::intent(
                        "Close this tab",
                        format!("tab.close:{}:{}", gid, idx),
                    ));
                    push_group_new_entry(&mut entries, gid);
                }
                // Placeholder-specific: quick access to agent picker.
                if let Some(pane) = self.panes.get(pane_id) {
                    if pane.title() == AGENT_PICKER_TITLE {
                        entries.push(crate::picker::PickerEntry::intent(
                            "Pick an agent…",
                            "agents.pick",
                        ));
                    }
                }
            }
            let anchor = crate::picker::PickerAnchor::Anchored {
                x: col,
                y: row,
                bounds: outer_rect,
            };
            self.picker_state.open_with_anchor("Pane", entries, anchor);
        }
    }

    /// The outer rect of `gid`'s active pane, i.e. what the tab strip
    /// sits above. Used to bound a context menu spawned from clicking
    /// on that strip. `None` when the group has no live active pane.
    fn group_bounds(&self, gid: TabGroupId) -> Option<Rect> {
        let group = self.tree.find_tab_group(gid)?;
        let active = group.active_pane()?;
        self.last_pane_outer_rects
            .iter()
            .find(|(id, _)| *id == active)
            .map(|(_, r)| *r)
    }

    fn mouse_drag(&mut self, col: u16, row: u16) {
        let Some(drag) = self.active_drag.clone() else {
            return;
        };
        if drag.parent_extent == 0 {
            return;
        }
        let now = match drag.axis {
            Direction::Horizontal => col,
            Direction::Vertical => row,
        };
        let cell_delta = now as i32 - drag.origin_axis_coord as i32;
        // Delta ratio from cell delta relative to the parent's extent.
        let delta_ratio = cell_delta as f32 / drag.parent_extent as f32;
        // Reset ratios to baseline, then apply once. This keeps the drag
        // idempotent across many Drag events without accumulating rounding.
        if self
            .tree
            .set_ratios(&drag.path, drag.baseline_ratios.clone())
            .is_err()
        {
            return;
        }
        let floors = min_size_floors(&self.tree, &drag.path, drag.parent_extent);
        match self
            .tree
            .adjust_ratio(&drag.path, drag.boundary, delta_ratio, &floors)
        {
            Ok(()) => {}
            Err(rimeterm_core::layout::RatioError::BelowMinSize) => {
                // Silently clamp at the floor: keep baseline in effect until
                // the user moves back into a valid range.
                self.set_hint("⛔ at minimum size".into());
            }
            Err(_) => {}
        }
    }

    fn draw(&mut self, area: Rect, buf: &mut ratatui::buffer::Buffer) -> Option<(u16, u16)> {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // status
                Constraint::Min(1), // pane area (each group's rect gets an internal tab strip row)
                Constraint::Length(1), // hint bar
            ])
            .split(area);

        let ws_label = self
            .workspace_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(workspace)");
        render_status_bar(vertical[0], buf, ws_label, &self.shell_short);
        // Cache current-frame geometry so mouse hit-tests use the same
        // rects the user is looking at.
        self.last_pane_area = vertical[1];
        self.last_dividers = self.tree.dividers(vertical[1]);
        self.last_tab_strips.clear();
        self.last_pane_outer_rects.clear();
        // Filled by the focused pane's render (see below). Overlays
        // (menu/palette/picker) override to `None` at the end of draw
        // so the caret doesn't leak past them.
        let mut focused_cursor: Option<(u16, u16)> = None;

        // Compute the rect for each *tab group cell*, then split off a 1-row
        // tab strip inside each cell. This is simpler than tracking tab strips
        // as separate layout nodes and keeps the LayoutTree pure.
        let group_ids = [
            BUILTIN_FILES,
            BUILTIN_SYSMON,
            BUILTIN_AGENTS,
            BUILTIN_SHELLS,
        ];
        for gid in group_ids {
            let Some(cell) = group_cell_rect(&self.tree, vertical[1], gid) else {
                continue;
            };
            let inner = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(cell);
            let strip_rect = inner[0];
            let pane_rect = inner[1];

            if let Some(group) = self.tree.find_tab_group(gid) {
                let titles: Vec<String> = group
                    .members()
                    .iter()
                    .map(|id| {
                        self.panes
                            .get(*id)
                            .map(|p| p.title().to_owned())
                            .unwrap_or_else(|| "(gone)".into())
                    })
                    .collect();
                let hits = crate::tab_strip::hit_rects(strip_rect, group, &titles);
                self.last_tab_strips.push((gid, hits));
                render_tab_strip(strip_rect, buf, group, &titles);
                if let Some(active_id) = group.active_pane() {
                    self.last_pane_outer_rects.push((active_id, pane_rect));
                    if let Some(pane) = self.panes.get_mut(active_id) {
                        let focused = self.focus.focused_pane() == Some(active_id);
                        let ctx = PaneRenderCtx {
                            focused,
                            title_override: None,
                        };
                        let outcome = pane.render(pane_rect, buf, &ctx);
                        // Only the focused pane's caret request is
                        // captured — every other pane's `cursor` is
                        // discarded. Overlays (menu/palette/picker)
                        // override this at the end of draw().
                        if focused {
                            focused_cursor = outcome.cursor;
                        }
                    }
                }
            }
        }

        // Divider hover overlay (§C16): terminals don't let us change
        // the OS mouse cursor to a resize glyph, so we tint the seam
        // bright + bold when the pointer is on it. The seam cells
        // already hold pane-border glyphs (`│` for vertical splits, `─`
        // for horizontal) — we just replace their style, keeping the
        // character intact so the frame stays visually consistent.
        //
        // Two guards:
        // 1. Skip overlay entirely during an active drag. The drag
        //    itself is the affordance and `hovered_divider.rect` is
        //    frozen from the pre-drag hover — painting it during drag
        //    leaves yellow pollution on cells the seam has already
        //    moved away from.
        // 2. Re-lookup the CURRENT rect from `last_dividers` keyed by
        //    (path, boundary). Ratios change over time (keyboard
        //    resize, layout reset, terminal resize) and the cached
        //    rect inside `hovered_divider` is only trustworthy on the
        //    frame it was recorded. Fresh lookup on every draw keeps
        //    the overlay in lockstep with reality.
        let live_hover = live_hover_overlay(
            self.active_drag.is_some(),
            self.hovered_divider.as_ref(),
            &self.last_dividers,
        );
        if let Some((seam_rect, _)) = live_hover {
            let style = Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::BOLD);
            for y in seam_rect.y..seam_rect.y.saturating_add(seam_rect.height) {
                for x in seam_rect.x..seam_rect.x.saturating_add(seam_rect.width) {
                    // Defensive: skip cells outside the terminal grid
                    // (e.g. right after a shrink resize).
                    if x >= area.x.saturating_add(area.width)
                        || y >= area.y.saturating_add(area.height)
                    {
                        continue;
                    }
                    buf[(x, y)].set_style(style);
                }
            }
        }

        // Hint bar precedence (highest → lowest):
        //   1. `pending_spawn`  — spawn spinner. Highest priority
        //      because the user JUST pressed Enter on a picker row and
        //      needs immediate feedback the terminal isn't hung.
        //   2. Live divider hover — the pointer is on a seam and there's
        //      no way to change the OS cursor from a terminal, so we
        //      commandeer the hint bar as the affordance channel.
        //   3. Transient `self.hint` — set_hint() messages, ~3s TTL.
        //   4. Default keybind row.
        //
        // Style also stacks: pending_spawn renders bright (not DIM) so
        // it clearly reads as "something's happening"; everything else
        // stays dim.
        let (hint_text, hint_style) = if let Some(pending) = &self.pending_spawn {
            let elapsed = pending.started.elapsed();
            let text = format!(
                "{} Initializing {}…  ({:.1}s)",
                spinner_glyph(elapsed),
                pending.label,
                elapsed.as_secs_f32(),
            );
            (
                text,
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else if let Some((_, axis)) = live_hover {
            let text = match axis {
                Direction::Horizontal => {
                    "↔ drag to resize · Ctrl+Alt+R for keyboard resize · right-click for menu"
                        .to_string()
                }
                Direction::Vertical => {
                    "↕ drag to resize · Ctrl+Alt+R for keyboard resize · right-click for menu"
                        .to_string()
                }
            };
            (text, Style::default().add_modifier(Modifier::DIM))
        } else {
            let text = self
                .hint
                .as_ref()
                .map(|(m, _)| m.clone())
                .unwrap_or_else(hint_bar_text);
            (text, Style::default().add_modifier(Modifier::DIM))
        };
        Paragraph::new(Line::from(hint_text))
            .style(hint_style)
            .render(vertical[2], buf);

        if self.menu_state.open {
            let rect = menu_rect(area, &self.menu);
            render_menu(rect, buf, &self.menu_state, &self.menu);
        }
        if self.palette_state.open {
            let rect = palette_rect(area);
            let entries = self.command_entries();
            render_palette(rect, buf, &self.palette_state, &entries);
        }
        if self.picker_state.open {
            let rect = crate::picker::popup_rect(area, &self.picker_state);
            crate::picker::render(rect, buf, &self.picker_state);
        }

        // Suppress the caret when any overlay owns the input focus.
        // Menu/palette/picker draw their own selection markers; a
        // stray shell caret bleeding through would be confusing.
        let cursor = if self.menu_state.open || self.palette_state.open || self.picker_state.open {
            None
        } else {
            focused_cursor
        };

        // Snapshot state for IPC consumers (§11). Cheap: reads &self + writes
        // a small owned struct; no PTY I/O.
        self.refresh_snapshot();
        cursor
    }

    /// Apply every pending IPC mutation, sending the outcome on each
    /// mutation's ack sender. Called once per tick before `drain_flags` so
    /// changes are visible in the same frame.
    fn drain_mutations(&mut self) {
        // Move the queue out under the short lock so mutations that touch
        // App can't re-enter the queue via ipc (would deadlock).
        let batch: Vec<PaneMutation> = {
            let mut q = self.pending_mutations.lock();
            q.drain(..).collect()
        };
        if batch.is_empty() {
            return;
        }
        // Buffer outcomes so we can refresh the snapshot ONCE after every
        // mutation applies but BEFORE any ack fires. Otherwise the caller
        // races us to `workspace.snapshot` and reads pre-mutation state.
        enum Ack {
            Unit(
                std::sync::mpsc::SyncSender<Result<(), String>>,
                Result<(), String>,
            ),
            U64(
                std::sync::mpsc::SyncSender<Result<u64, String>>,
                Result<u64, String>,
            ),
        }
        let mut acks: Vec<Ack> = Vec::with_capacity(batch.len());
        for mutation in batch {
            match mutation {
                PaneMutation::Close { pane_id, ack } => {
                    let outcome = self.close_pane_by_id(pane_id).map_err(|e| e.to_string());
                    acks.push(Ack::Unit(ack, outcome));
                }
                PaneMutation::OpenShell { ack } => {
                    let outcome = self
                        .new_shell_tab_in(BUILTIN_SHELLS)
                        .map(|id| id.0)
                        .map_err(|e| e.to_string());
                    acks.push(Ack::U64(ack, outcome));
                }
                PaneMutation::OpenAgent { spec, ack } => {
                    // Grab the label BEFORE moving `spec` into the mutation
                    // call so we can attach it to the hint-bar spinner if
                    // the spawn succeeds. `spec` is a `&'static AgentSpec`
                    // so `.label` is Copy on the &str reference.
                    let label = spec.label;
                    let outcome = self
                        .new_agent_tab_in(BUILTIN_AGENTS, spec)
                        .map(|id| id.0)
                        .map_err(|e| e.to_string());
                    if let Ok(pane_id_num) = outcome {
                        // Coding-agent CLIs take multiple seconds to
                        // print their first prompt. Show a hint-bar
                        // spinner until output arrives; cleared in
                        // `expire_pending_spawn`.
                        self.pending_spawn = Some(PendingSpawn {
                            label: label.to_string(),
                            pane_id: PaneId(pane_id_num),
                            started: Instant::now(),
                        });
                    }
                    acks.push(Ack::U64(ack, outcome));
                }
                PaneMutation::Rename {
                    pane_id,
                    title,
                    ack,
                } => {
                    let outcome = self
                        .rename_pane_by_id(pane_id, title)
                        .map_err(|e| e.to_string());
                    acks.push(Ack::Unit(ack, outcome));
                }
                PaneMutation::Focus { pane_id, ack } => {
                    let outcome = self.focus_pane_by_id(pane_id).map_err(|e| e.to_string());
                    acks.push(Ack::Unit(ack, outcome));
                }
                PaneMutation::OpenShellAndType { command } => {
                    match self.new_shell_tab_in(BUILTIN_SHELLS) {
                        Ok(new_id) => {
                            // Type the command straight into the fresh
                            // shell. NO Enter — the user reviews and hits
                            // Enter themselves (the whole point of routing
                            // through a shell instead of Command::spawn).
                            let session = self.session_writes.lock().get(&new_id).cloned();
                            if let Some(session) = session {
                                if let Err(e) = session.write(command.as_bytes()) {
                                    tracing::warn!(
                                        pane_id = new_id.0,
                                        error = %e,
                                        "failed to inject install command"
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    pane_id = new_id.0,
                                    "new shell has no session_writes entry (race?)"
                                );
                            }
                        }
                        Err(e) => {
                            self.set_hint(format!("⛔ install shortcut: {}", e));
                        }
                    }
                }
            }
        }
        // Publish the post-mutation state THEN wake the waiting clients;
        // any snapshot request that lands after their ack sees fresh data.
        self.refresh_snapshot();
        for a in acks {
            match a {
                Ack::Unit(tx, r) => {
                    // Rx may be gone (client timed out). Fine — drop.
                    let _ = tx.send(r);
                }
                Ack::U64(tx, r) => {
                    let _ = tx.send(r);
                }
            }
        }
    }

    fn drain_flags(&mut self) {
        let f = Arc::clone(&self.flags);
        if f.menu_toggle.swap(false, Ordering::Relaxed) {
            self.menu_state.toggle();
        }
        if f.palette_toggle.swap(false, Ordering::Relaxed) {
            if self.palette_state.open {
                self.palette_state.close();
            } else {
                self.palette_state.open();
            }
        }
        if f.tab_next.swap(false, Ordering::Relaxed) {
            self.tab_step(true);
        }
        if f.tab_prev.swap(false, Ordering::Relaxed) {
            self.tab_step(false);
        }
        let goto = f.tab_goto.swap(0, Ordering::Relaxed);
        if goto > 0 {
            self.tab_goto(goto - 1);
        }
        let dir = f.focus_dir.swap(0, Ordering::Relaxed);
        if dir > 0 {
            self.focus_direction(dir);
        }
        let quad = f.focus_quadrant.swap(0, Ordering::Relaxed);
        if quad > 0 {
            self.focus_quadrant(quad);
        }
        if f.shells_new.swap(false, Ordering::Relaxed) {
            // Ctrl+T is context-sensitive:
            // - focused group = shells → spawn a new shell tab (C1 behavior)
            // - focused group = agents → open the picker dropdown (agents
            //   have 4 possible providers, so a menu beats an implicit spawn)
            // - anywhere else → surface a "Fixed group" hint via new_shell_tab.
            let focused = self.focus.focused_group();
            if focused == Some(BUILTIN_AGENTS) {
                self.open_agent_picker();
            } else if let Err(e) = self.new_shell_tab() {
                self.set_hint(format!("⛔ {}", e));
            }
        }
        if f.shells_close.swap(false, Ordering::Relaxed) {
            if let Err(e) = self.close_current_shell_tab() {
                self.set_hint(format!("⛔ {}", e));
            }
        }
        if f.resize_toggle.swap(false, Ordering::Relaxed) {
            self.resize_mode = !self.resize_mode;
            let msg = if self.resize_mode {
                "Resize mode: H/L/K/J adjust · Shift = ×5 · = restore · Esc/Enter exit"
            } else {
                "Resize mode: off"
            };
            self.set_hint(msg.into());
        }
        if f.layout_reset.swap(false, Ordering::Relaxed) {
            self.reset_layout();
        }
        if f.settings.swap(false, Ordering::Relaxed) {
            info!("app.settings fired (v0.1 stub: log only)");
            self.set_hint("Settings will open the config in a system editor (M3+)".into());
        }
        if f.acknowledgement.swap(false, Ordering::Relaxed) {
            info!("app.acknowledgement fired (v0.1 stub: log only)");
            self.set_hint("Acknowledgement will open ACKNOWLEDGEMENTS.md (M3+)".into());
        }
    }

    fn tab_step(&mut self, forward: bool) {
        let Some(gid) = self.focus.focused_group() else {
            return;
        };
        if let Some(group) = self.tree.find_tab_group_mut(gid) {
            if forward {
                group.next();
            } else {
                group.prev();
            }
            if let Some(id) = group.active_pane() {
                self.focus.set_focus(id, Some(gid));
            }
        }
    }

    fn tab_goto(&mut self, idx: usize) {
        let Some(gid) = self.focus.focused_group() else {
            return;
        };
        if let Some(group) = self.tree.find_tab_group_mut(gid) {
            if group.goto(idx).is_ok() {
                if let Some(id) = group.active_pane() {
                    self.focus.set_focus(id, Some(gid));
                }
            } else {
                self.set_hint(format!("no tab {} in {}", idx + 1, gid));
            }
        }
    }

    fn focus_direction(&mut self, dir: usize) {
        // dir: 1=left 2=right 3=up 4=down
        let current = self.focus.focused_group();
        let target = current.and_then(|g| neighbor_group(g, dir));
        if let Some(gid) = target {
            self.focus_group(gid);
        }
    }

    fn focus_quadrant(&mut self, quad: usize) {
        let gid = match quad {
            1 => BUILTIN_FILES,
            2 => BUILTIN_AGENTS,
            3 => BUILTIN_SYSMON,
            4 => BUILTIN_SHELLS,
            _ => return,
        };
        self.focus_group(gid);
    }

    fn focus_group(&mut self, gid: TabGroupId) {
        let Some(group) = self.tree.find_tab_group(gid) else {
            return;
        };
        if let Some(id) = group.active_pane() {
            self.focus.set_focus(id, Some(gid));
        }
    }

    fn new_shell_tab(&mut self) -> Result<PaneId> {
        let gid = self
            .focus
            .focused_group()
            .ok_or_else(|| anyhow!("no focused group"))?;
        self.new_shell_tab_in(gid)
    }

    /// Spawn a fresh shell tab in `gid` regardless of focus. Returns the new
    /// `PaneId` so IPC callers can address it. Used by both `Ctrl+T`
    /// (focused group) and `workspace.pane.open` (currently only shells;
    /// agents will need their own kind in the M4 milestone).
    fn new_shell_tab_in(&mut self, gid: TabGroupId) -> Result<PaneId> {
        // Only shells accepts new tabs (spec §19.10.10). Return the policy
        // error verbatim so the status bar shows the reason.
        let group = self
            .tree
            .find_tab_group(gid)
            .ok_or_else(|| anyhow!("group {} missing", gid))?;
        match group.policy() {
            MembersPolicy::Fixed => return Err(anyhow!("{} is fixed; cannot add tabs", gid)),
            MembersPolicy::Open { .. } => {}
        }
        if group.kind() != PaneKind::Shell {
            // For now the only Open kind we can spawn is Shell (agents will
            // land in later milestones).
            return Err(anyhow!("Ctrl+T not yet supported for {}", gid));
        }

        let next_num = next_shell_number(group.members(), &self.panes);
        let display = format!("shell-{}", next_num);
        let spawn = spawn_shell(
            &self.shell_choice,
            self.workspace_root.clone(),
            display,
            80,
            24,
            self.redraw_tx.clone(),
        )?;
        let new_id = spawn.pane.id();
        self.session_writes
            .lock()
            .insert(new_id, spawn.pane.session().clone());
        self.panes.insert(Box::new(spawn.pane));

        let group = self.tree.find_tab_group_mut(gid).expect("group present");
        group
            .try_add(new_id, PaneKind::Shell)
            .map_err(|e| anyhow!("policy rejected new tab: {e}"))?;
        self.focus.set_focus(new_id, Some(gid));
        Ok(new_id)
    }

    /// Spawn a fresh agent tab in `gid`. Static `AgentSpec` comes from
    /// [`rimeterm_pty::agent_registry`] via `parse_open_args`. Reuses
    /// [`build_agent_pane`] so PTY / placeholder routing is identical to
    /// the pre-configured `[agents.tabs]` path.
    fn new_agent_tab_in(
        &mut self,
        gid: TabGroupId,
        spec: &'static rimeterm_pty::agent_registry::AgentSpec,
    ) -> Result<PaneId> {
        let group = self
            .tree
            .find_tab_group(gid)
            .ok_or_else(|| anyhow!("group {} missing", gid))?;
        match group.policy() {
            MembersPolicy::Fixed => return Err(anyhow!("{} is fixed; cannot add tabs", gid)),
            MembersPolicy::Open { .. } => {}
        }
        if group.kind() != PaneKind::AgentChat {
            return Err(anyhow!(
                "kind=agent:<id> only valid on agents group; {} takes shells",
                gid
            ));
        }
        // Build the config-side spec on the fly; the registry entry has
        // everything build_agent_pane needs.
        let external_spec = rimeterm_config::AgentSpec {
            id: spec.id.to_string(),
            label: spec.label.to_string(),
            command: spec.argv.iter().map(|s| s.to_string()).collect(),
            install_hint: Some(spec.install_hint.to_string()),
        };
        let new_id = build_agent_pane(
            &mut self.panes,
            &self.session_writes,
            &external_spec,
            &self.workspace_root,
            self.redraw_tx.clone(),
        )?;
        self.pane_agent_id.insert(new_id, spec.id);

        // If the group is still holding the picker-placeholder from
        // first-launch, remove it so the new agent tab is the sole (and
        // active) entry. TabGroup::try_close refuses to remove the last
        // member without `force`, so add first then close.
        let group = self.tree.find_tab_group_mut(gid).expect("group present");
        group
            .try_add(new_id, PaneKind::AgentChat)
            .map_err(|e| anyhow!("policy rejected new agent tab: {e}"))?;
        // Sweep any placeholder(s) whose pane provider is `AgentPicker`.
        let sweep: Vec<PaneId> = group
            .members()
            .iter()
            .copied()
            .filter(|id| {
                self.panes
                    .get(*id)
                    .map(|p| p.title() == AGENT_PICKER_TITLE)
                    .unwrap_or(false)
            })
            .collect();
        for placeholder_id in sweep {
            if let Some(idx) = group.members().iter().position(|m| *m == placeholder_id) {
                // Use `force: true` because the placeholder might be the
                // last non-agent member; the new agent tab is already in.
                let _ = group.try_close(idx, true);
                drop_pane(&mut self.panes, placeholder_id);
            }
        }
        self.focus.set_focus(new_id, Some(gid));
        self.persist_agents_state();
        Ok(new_id)
    }

    fn close_current_shell_tab(&mut self) -> Result<()> {
        let gid = self
            .focus
            .focused_group()
            .ok_or_else(|| anyhow!("no focused group"))?;
        let group = self
            .tree
            .find_tab_group(gid)
            .ok_or_else(|| anyhow!("group {} missing", gid))?;
        let idx = group.active_index();
        self.close_tab_in_group(gid, idx)?;
        Ok(())
    }

    /// Close whichever tab holds `pane_id`. IPC entry point; walks every
    /// `TabGroup` to find the owner. Fails if the owning group is
    /// [`MembersPolicy::Fixed`] or if it would leave the group empty.
    fn close_pane_by_id(&mut self, pane_id: PaneId) -> Result<()> {
        let (gid, idx) = self
            .tree
            .tab_groups()
            .iter()
            .find_map(|g| {
                g.members()
                    .iter()
                    .position(|m| *m == pane_id)
                    .map(|i| (g.id(), i))
            })
            .ok_or_else(|| anyhow!("pane {} not in any tab group", pane_id.0))?;
        self.close_tab_in_group(gid, idx)
    }

    /// Shared close routine. `idx` is the position of the tab inside `gid`'s
    /// members. Policy errors bubble as-is so the caller can surface them.
    fn close_tab_in_group(&mut self, gid: TabGroupId, idx: usize) -> Result<()> {
        let group = self
            .tree
            .find_tab_group_mut(gid)
            .ok_or_else(|| anyhow!("group {} missing", gid))?;
        match group.policy() {
            MembersPolicy::Fixed => return Err(anyhow!("{} is fixed; cannot close tabs", gid)),
            MembersPolicy::Open { .. } => {}
        }
        let removed = group.try_close(idx, false).map_err(|e| anyhow!("{e}"))?;
        drop_pane(&mut self.panes, removed);
        self.session_writes.lock().remove(&removed);
        // Clean the reverse lookup + persist if we just changed the
        // agents quadrant. Persisting is idempotent + cheap (single
        // TOML file, few bytes) so we do it unconditionally on any
        // agents-group mutation rather than trying to gate it further.
        let was_agent = self.pane_agent_id.remove(&removed).is_some();
        if let Some(group) = self.tree.find_tab_group(gid) {
            if let Some(id) = group.active_pane() {
                self.focus.set_focus(id, Some(gid));
            }
        }
        if was_agent || gid == BUILTIN_AGENTS {
            self.persist_agents_state();
        }
        Ok(())
    }

    /// Rename a pane in place via [`PaneProvider::set_title`]. Fails if
    /// the pane doesn't exist or the provider refused the rename.
    fn rename_pane_by_id(&mut self, pane_id: PaneId, title: String) -> Result<()> {
        let pane = self
            .panes
            .get_mut(pane_id)
            .ok_or_else(|| anyhow!("pane {} not found", pane_id.0))?;
        if !pane.set_title(title) {
            return Err(anyhow!(
                "pane {} refused rename (provider not renamable)",
                pane_id.0
            ));
        }
        Ok(())
    }

    /// Focus a pane by id. Walks the tab tree to find the owning group,
    /// activates the corresponding tab, and moves the focus manager.
    fn focus_pane_by_id(&mut self, pane_id: PaneId) -> Result<()> {
        let (gid, idx) = self
            .tree
            .tab_groups()
            .iter()
            .find_map(|g| {
                g.members()
                    .iter()
                    .position(|m| *m == pane_id)
                    .map(|i| (g.id(), i))
            })
            .ok_or_else(|| anyhow!("pane {} not in any tab group", pane_id.0))?;
        // Update the tab group's active index. `goto` is fallible for
        // out-of-range but we just computed the index from members, so it
        // won't fail — still bubble to be defensive.
        if let Some(group) = self.tree.find_tab_group_mut(gid) {
            group.goto(idx).map_err(|e| anyhow!("{e}"))?;
        }
        self.focus.set_focus(pane_id, Some(gid));
        Ok(())
    }

    fn command_entries(&self) -> Vec<CommandEntry> {
        self.commands
            .iter()
            .map(CommandEntry::from_command)
            .collect()
    }

    /// Rebuild the shared [`WorkspaceSnapshot`] from live state. Called at
    /// the end of every frame; workspace.snapshot IPC returns whatever this
    /// produced last.
    fn refresh_snapshot(&self) {
        let mut snap = WorkspaceSnapshot {
            focused_group: self.focus.focused_group().map(|g| g.as_str()),
            focused_pane_id: self.focus.focused_pane().map(|p| p.0),
            groups: Vec::new(),
            workspace_root: self.workspace_root.display().to_string(),
            shell_short: self.shell_short.clone(),
        };
        let sessions = self.session_writes.lock();
        for group in self.tree.tab_groups() {
            let active_idx = group.active_index();
            let mut tabs = Vec::new();
            for (idx, id) in group.members().iter().enumerate() {
                let title = self
                    .panes
                    .get(*id)
                    .map(|p| p.title().to_owned())
                    .unwrap_or_else(|| "(gone)".into());
                tabs.push(TabSnapshot {
                    pane_id: id.0,
                    title,
                    is_active: idx == active_idx,
                    has_pty: sessions.contains_key(id),
                });
            }
            snap.groups.push(TabGroupSnapshot {
                id: group.id().as_str(),
                active_tab_index: active_idx,
                tabs,
            });
        }
        *self.snapshot.write() = snap;
    }

    fn set_hint(&mut self, msg: String) {
        self.hint = Some((msg, Instant::now()));
    }

    fn expire_hint(&mut self) {
        if let Some((_, t)) = &self.hint {
            if t.elapsed() > Duration::from_secs(3) {
                self.hint = None;
            }
        }
    }

    /// Clear the boot-progress spinner if either the target pane has
    /// produced first output or the timeout deadline hit. Called each
    /// tick alongside `expire_hint`. The classification decision is
    /// factored out into the pure [`pending_spawn_should_clear`] so tests
    /// can drive every branch without a live PTY.
    ///
    /// **Historic bug fix**: previous versions sampled the LAST four
    /// rows of the grid via `grid_contents(Some(4))`. Full-screen alt-
    /// screen TUIs (claude, codex, omp) paint their banner at the top
    /// and leave the bottom rows blank — so the sample was whitespace
    /// forever and the spinner only cleared after the user forced a
    /// re-render (window resize, which triggers the child to repaint
    /// its whole viewport, at which point the bottom rows finally hold
    /// a status bar / prompt). We now sample the ENTIRE visible viewport.
    fn expire_pending_spawn(&mut self) {
        let Some(pending) = &self.pending_spawn else {
            return;
        };
        // Sample the whole visible viewport (rows = None). Cheap: a
        // single `parking_lot::Mutex` lock + a String walk sized to
        // (cols × rows), typically < 20 KiB. Runs at ~60 Hz only while a
        // spawn is pending — once cleared, the outer `let Some(...)`
        // returns without touching the mutex.
        let sample = self
            .session_writes
            .lock()
            .get(&pending.pane_id)
            .map(|s| s.grid_contents(None));
        if pending_spawn_should_clear(pending.started.elapsed(), sample.as_deref()) {
            self.pending_spawn = None;
            // Force one more render immediately — without this pulse the
            // hint bar would keep drawing "Initializing …" until the next
            // input/timer tick delivers a fresh frame. The 16ms fallback
            // in the main loop usually saves us, but under a busy tokio
            // runtime or a starved timer wheel it can take noticeably
            // longer. Belt-and-braces: pulse once, cost = one channel send.
            let _ = self.redraw_tx.send(());
        }
    }

    /// Force any throttled PTY resizes to apply immediately. Called on
    /// mouse-up (§19.12.6) and any time the app needs the final drag size
    /// to land exactly.
    fn flush_pending_resizes(&mut self) {
        let ids: Vec<PaneId> = self
            .tree
            .tab_groups()
            .iter()
            .flat_map(|g| g.members().iter().copied())
            .collect();
        for id in ids {
            if let Some(pane) = self.panes.get_mut(id) {
                pane.flush_pending_resize();
            }
        }
    }

    async fn spawn_ipc_server(&self) -> Option<tokio::sync::mpsc::Sender<()>> {
        let pid = std::process::id();
        let commands = std::sync::Arc::clone(&self.commands);
        let handler: rimeterm_ipc::Handler =
            std::sync::Arc::new(move |req: rimeterm_ipc::Request| {
                // Match `req.cmd` against a registered command id. The registry
                // keys are `&'static str` literals so the lookup is a simple
                // linear scan; the command set stays tiny (<50 entries in M6).
                let matched: Option<&'static str> =
                    commands.iter().find(|c| c.id == req.cmd).map(|c| c.id);
                let Some(id) = matched else {
                    return rimeterm_ipc::Response::err(format!("unknown command `{}`", req.cmd));
                };
                match commands.run_with(id, &req.args) {
                    Ok(result) => rimeterm_ipc::Response::success(result),
                    Err(e) => rimeterm_ipc::Response::err(e.to_string()),
                }
            });
        match rimeterm_ipc::spawn(pid, handler).await {
            Ok(tx) => {
                if let Some(ep) = rimeterm_ipc::endpoint_display_for_pid(pid) {
                    tracing::info!(endpoint = %ep, pid = pid, "ipc server listening");
                }
                Some(tx)
            }
            Err(e) => {
                tracing::warn!(error = %e, "ipc server failed to start");
                None
            }
        }
    }

    fn shutdown(&mut self) {
        let all: Vec<PaneId> = self
            .tree
            .tab_groups()
            .iter()
            .flat_map(|g| g.members().iter().copied())
            .collect();

        // Persist current ratios (§19.12.9). Silent on error — persistence
        // is a nice-to-have; we should never block shutdown on it.
        self.persist_layout();
        for id in all {
            drop_pane(&mut self.panes, id);
            self.session_writes.lock().remove(&id);
        }
    }

    /// Write the current split ratios to the workspace's layout.state.toml.
    fn persist_layout(&self) {
        let Some(path) = rimeterm_config::layout_state::workspace_state_file(&self.workspace_root)
        else {
            return;
        };
        let state = snapshot_persisted_state(&self.tree);
        if let Err(e) = state.save_to(&path) {
            warn!(error = %e, "failed to persist layout state");
        } else {
            info!(path = %path.display(), "persisted layout state");
        }
    }

    /// Write the current agents-quadrant tab list to
    /// `${data_dir}/workspaces/<hash>/agents.state.toml`. Silent on
    /// error — the next launch just won't restore, no user harm.
    fn persist_agents_state(&self) {
        let Some(path) = rimeterm_config::agents_state::workspace_state_file(&self.workspace_root)
        else {
            return;
        };
        // Walk the agents group in tab order so on-disk order matches
        // on-screen order. Placeholder panes (no entry in pane_agent_id)
        // are skipped — persisting the picker itself would defeat the
        // whole restore contract.
        let tabs: Vec<String> = self
            .tree
            .find_tab_group(BUILTIN_AGENTS)
            .map(|g| {
                g.members()
                    .iter()
                    .filter_map(|pid| self.pane_agent_id.get(pid).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let state = rimeterm_config::agents_state::AgentsState { tabs };
        if let Err(e) = state.save_to(&path) {
            warn!(error = %e, "failed to persist agents state");
        } else {
            info!(path = %path.display(), count = state.tabs.len(), "persisted agents state");
        }
    }

    /// Reset every split ratio to defaults and delete the persisted state file.
    fn reset_layout(&mut self) {
        for (path, ratios) in self.default_ratios.clone() {
            let _ = self.tree.set_ratios(&path, ratios);
        }
        if let Some(path) =
            rimeterm_config::layout_state::workspace_state_file(&self.workspace_root)
        {
            let _ = std::fs::remove_file(&path);
        }
        self.set_hint("layout reset to defaults (persisted state cleared)".into());
    }
}

fn pick_shell(config: &Config) -> Result<ShellChoice> {
    let hints: &[String] = if cfg!(windows) {
        &config.core.shell_win
    } else {
        &config.core.shell_unix
    };
    let choice = detect_default_shell(hints);
    if choice == ShellChoice::None {
        Err(anyhow!(
            "no shell found; tried hints={:?}",
            hints.iter().collect::<Vec<_>>()
        ))
    } else {
        Ok(choice)
    }
}

/// Insert either a real spawned external PTY (if `spec.command[0]` is on
/// PATH) or a styled placeholder pane telling the user how to install it.
///
/// **Design decision (v0.2)**: agents, file managers, git TUIs and system
/// monitors are all external binaries. rimeterm does not bundle them.
/// Missing → placeholder + install hint; present → PTY child.
fn build_external_pane(
    panes: &mut PaneRegistry,
    session_writes: &parking_lot::Mutex<std::collections::HashMap<PaneId, rimeterm_pty::Session>>,
    spec: &rimeterm_config::ExternalToolSpec,
    workspace_root: &std::path::Path,
    redraw: mpsc::UnboundedSender<()>,
    icon: &str,
    color: Color,
    kind_label: &str,
) -> Result<PaneId> {
    match rimeterm_pty::detect_tool(&spec.command) {
        rimeterm_pty::ToolAvailability::Available(program) => {
            let args: Vec<String> = spec.command.iter().skip(1).cloned().collect();
            let spawn = crate::agent_factory::spawn_external(
                program,
                args,
                workspace_root.to_path_buf(),
                spec.id.clone(),
                80,
                24,
                redraw,
            )?;
            let id = spawn.pane.id();
            // Store the cloneable Session handle before we consume the pane
            // so IPC (workspace.pane.write) can write directly to this PTY.
            session_writes
                .lock()
                .insert(id, spawn.pane.session().clone());
            panes.insert(Box::new(spawn.pane));
            info!(
                kind = kind_label,
                id = spec.id.as_str(),
                label = spec.label.as_str(),
                "external tool spawned"
            );
            Ok(id)
        }
        rimeterm_pty::ToolAvailability::Missing { probed } => {
            info!(
                kind = kind_label,
                id = spec.id.as_str(),
                missing = probed.as_str(),
                "external tool not installed; showing placeholder"
            );
            // Stack: bold "not installed" heading, blank line, then the
            // multi-line InstallHint block. `PlaceholderPane` splits on
            // '\n' and left-aligns the rest.
            let subtitle = match spec.install_hint.as_deref() {
                Some(hint) if !hint.is_empty() => {
                    format!("not installed — `{}` not on PATH\n\n{}", probed, hint)
                }
                _ => format!("not installed — `{}` not on PATH", probed),
            };
            // Try to find a matching tools registry entry so we can offer
            // one-key install via `[I]`. Registry membership is what
            // enables `tools.install <name>` too, so this stays consistent.
            let mut pane =
                PlaceholderPane::new(spec.id.clone(), subtitle, icon.to_owned(), Color::DarkGray);
            if let Some(reg) = rimeterm_config::tools::find(&spec.id) {
                // Cross-platform default: `cargo install --locked <crate...>`.
                // Users on Windows / macOS see the multi-path hint on-screen
                // and can pick a different one manually; `[I]` just picks
                // the guaranteed-to-work path.
                let cmd = format!("cargo install --locked {}", reg.crates.join(" "));
                pane = pane.with_install_command(cmd);
            }
            let _ = color; // reserved for future available-pane border color
            let id = pane.id();
            panes.insert(Box::new(pane));
            Ok(id)
        }
    }
}

/// Legacy alias for M3 callers.
fn build_agent_pane(
    panes: &mut PaneRegistry,
    session_writes: &parking_lot::Mutex<std::collections::HashMap<PaneId, rimeterm_pty::Session>>,
    spec: &rimeterm_config::AgentSpec,
    workspace_root: &std::path::Path,
    redraw: mpsc::UnboundedSender<()>,
) -> Result<PaneId> {
    build_external_pane(
        panes,
        session_writes,
        spec,
        workspace_root,
        redraw,
        "🤖",
        Color::LightMagenta,
        "agent",
    )
}

/// Locate the rect a tab group occupies inside the pane area.
fn group_cell_rect(tree: &LayoutTree, area: Rect, target: TabGroupId) -> Option<Rect> {
    for (pane_id, rect) in tree.compute_rects(area) {
        for g in tree.tab_groups() {
            if g.id() == target && g.members().contains(&pane_id) {
                // `pane_id` is the active member of `target`; its rect IS the
                // group cell (LayoutTree walker maps a `Tabs` node to its
                // active leaf's rect).
                return Some(rect);
            }
        }
    }
    None
}

/// Human-facing subtitle for the picker placeholder. Lists detected
/// agents first (green), then missing ones (grey with hint).
pub(crate) fn format_agent_picker_hint() -> String {
    let detected = rimeterm_pty::agent_registry::detect_all();
    let mut lines = Vec::new();
    lines.push("Ctrl+Shift+P → search `agents.pick.` to spawn one:".to_string());
    for a in &detected {
        if a.is_available() {
            lines.push(format!(
                "  ✓ {} ({}) at {}",
                a.label,
                a.id,
                a.detected_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }
    }
    let missing: Vec<_> = detected.iter().filter(|a| !a.is_available()).collect();
    if !missing.is_empty() {
        lines.push(String::new());
        lines.push("Missing:".to_string());
        for a in missing {
            lines.push(format!("  ✘ {} — {}", a.label, a.install_hint));
        }
    }
    lines.join("\n")
}

fn neighbor_group(from: TabGroupId, dir: usize) -> Option<TabGroupId> {
    // Layout (quadrants):
    //   ┌ files   │ agents ┐
    //   ├ sysmon  │ shells ┤
    let same = from;
    let out = match (dir, from) {
        // 1 = left
        (1, g) if g == BUILTIN_AGENTS => BUILTIN_FILES,
        (1, g) if g == BUILTIN_SHELLS => BUILTIN_SYSMON,
        // 2 = right
        (2, g) if g == BUILTIN_FILES => BUILTIN_AGENTS,
        (2, g) if g == BUILTIN_SYSMON => BUILTIN_SHELLS,
        // 3 = up
        (3, g) if g == BUILTIN_SYSMON => BUILTIN_FILES,
        (3, g) if g == BUILTIN_SHELLS => BUILTIN_AGENTS,
        // 4 = down
        (4, g) if g == BUILTIN_FILES => BUILTIN_SYSMON,
        (4, g) if g == BUILTIN_AGENTS => BUILTIN_SHELLS,
        _ => same,
    };
    if out == from { None } else { Some(out) }
}

fn register_commands(
    cmds: &mut CommandRegistry,
    flags: Arc<ActionFlags>,
    snapshot: Arc<parking_lot::RwLock<WorkspaceSnapshot>>,
    session_writes: Arc<
        parking_lot::Mutex<std::collections::HashMap<PaneId, rimeterm_pty::Session>>,
    >,
    pending_mutations: Arc<parking_lot::Mutex<std::collections::VecDeque<PaneMutation>>>,
    redraw_tx: mpsc::UnboundedSender<()>,
) -> Result<()> {
    let register = |cmds: &mut CommandRegistry, cmd: Command| -> Result<()> {
        cmds.register(cmd).map_err(|e| anyhow!("{e}"))
    };

    macro_rules! flag_cmd {
        ($cmds:ident, $id:expr, $title:expr, $desc:expr, $flags:ident . $field:ident) => {{
            let f = $flags.clone();
            register(
                $cmds,
                Command::signal(
                    $id,
                    $title,
                    Some($desc),
                    Arc::new(move || f.$field.store(true, Ordering::Relaxed)),
                ),
            )?;
        }};
    }

    flag_cmd!(
        cmds,
        "app.quit",
        "Quit rimeterm",
        "Kill sessions and exit",
        flags.quit
    );
    flag_cmd!(
        cmds,
        "app.menu.toggle",
        "Toggle app menu",
        "F10 / Alt+M",
        flags.menu_toggle
    );
    flag_cmd!(
        cmds,
        "app.palette.open",
        "Open command palette",
        "Ctrl+Shift+P",
        flags.palette_toggle
    );
    flag_cmd!(
        cmds,
        "app.settings",
        "Open Settings",
        "Edit rimeterm config",
        flags.settings
    );
    flag_cmd!(
        cmds,
        "app.acknowledgement",
        "Acknowledgement",
        "Show ACKNOWLEDGEMENTS.md",
        flags.acknowledgement
    );
    flag_cmd!(
        cmds,
        "workspace.tab.next",
        "Next tab in group",
        "Alt+]",
        flags.tab_next
    );
    flag_cmd!(
        cmds,
        "workspace.tab.prev",
        "Previous tab in group",
        "Alt+[",
        flags.tab_prev
    );
    flag_cmd!(
        cmds,
        "workspace.shells.new",
        "New shell tab",
        "Ctrl+T",
        flags.shells_new
    );
    flag_cmd!(
        cmds,
        "workspace.shells.close",
        "Close shell tab",
        "Ctrl+W",
        flags.shells_close
    );
    flag_cmd!(
        cmds,
        "app.resize.toggle",
        "Toggle Resize mode",
        "Ctrl+Alt+R",
        flags.resize_toggle
    );
    flag_cmd!(
        cmds,
        "workspace.layout.reset",
        "Reset layout ratios",
        "Restore & clear persisted state",
        flags.layout_reset
    );

    // Nine tab-goto commands (Alt+Shift+1..9 shortcuts).
    for (i, id) in crate::keymap::all_tab_goto_ids().iter().enumerate() {
        let f = flags.clone();
        let title = tab_goto_title(i);
        register(
            cmds,
            Command::signal(
                *id,
                title,
                Some("Alt+Shift+<N>"),
                Arc::new(move || f.tab_goto.store((i + 1) as usize, Ordering::Relaxed)),
            ),
        )?;
    }
    let _ = tab_goto_command_id(0); // keep API export used

    // Focus direction commands.
    for (id, dir_val, title, desc) in [
        ("workspace.focus.left", 1usize, "Focus left cell", "Alt+H"),
        ("workspace.focus.right", 2, "Focus right cell", "Alt+L"),
        ("workspace.focus.up", 3, "Focus upper cell", "Alt+K"),
        ("workspace.focus.down", 4, "Focus lower cell", "Alt+J"),
    ] {
        let f = flags.clone();
        register(
            cmds,
            Command::signal(
                id,
                title,
                Some(desc),
                Arc::new(move || f.focus_dir.store(dir_val, Ordering::Relaxed)),
            ),
        )?;
    }

    // Quadrant jump commands.
    for (idx, id) in QUADRANT_COMMANDS.iter().enumerate() {
        let f = flags.clone();
        let title = quadrant_title(idx);
        register(
            cmds,
            Command::signal(
                *id,
                title,
                Some("Alt+<N>"),
                Arc::new(move || {
                    f.focus_quadrant
                        .store((idx + 1) as usize, Ordering::Relaxed)
                }),
            ),
        )?;
    }

    // Live-state reporter: reads the shared WorkspaceSnapshot (refreshed each
    // frame) and returns it as JSON. Ignores args.
    {
        let snap = snapshot.clone();
        let cmd = Command {
            id: "workspace.snapshot",
            title: "Snapshot workspace state",
            description: Some("Return groups + focused tab as JSON"),
            run: Arc::new(move |_args: &serde_json::Value| {
                let s = snap.read().clone();
                serde_json::to_value(&s).map_err(|e| format!("serialize: {e}"))
            }),
        };
        register(cmds, cmd)?;
    }

    // Parametric goto: takes `{index: <1-based>}` and reuses the existing
    // atomic-flag pipeline so palette / keymap / rimectl land on the same
    // path. Wraps the value into flags.tab_goto (1..=9).
    {
        let f = flags.clone();
        let cmd = Command {
            id: "workspace.tab.goto",
            title: "Go to tab N in focused group",
            description: Some("args: {index: 1..=9}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let idx = args
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "missing `index` (u64 1..=9)".to_string())?;
                if !(1..=9).contains(&idx) {
                    return Err(format!("index {} out of range 1..=9", idx));
                }
                f.tab_goto.store(idx as usize, Ordering::Relaxed);
                Ok(serde_json::json!({"queued": idx}))
            }),
        };
        register(cmds, cmd)?;
    }

    // Drive any live PTY pane. `args = {pane_id: u64, text: String, enter?: bool}`.
    // Writes `text` (plus optional Enter) to the pane's PTY writer. Payload
    // capped at 4 KiB so a bad script can't hammer the pipe with megabytes.
    {
        let sw = session_writes.clone();
        let cmd = Command {
            id: "workspace.pane.write",
            title: "Write text to a pane's PTY",
            description: Some("args: {pane_id: u64, text: String, enter?: bool}"),
            run: Arc::new(move |args: &serde_json::Value| {
                const MAX_BYTES: usize = 4096;
                let pane_id_num = args
                    .get("pane_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "missing `pane_id` (u64)".to_string())?;
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing `text` (string)".to_string())?;
                if text.is_empty() {
                    return Err("`text` must not be empty".into());
                }
                if text.len() > MAX_BYTES {
                    return Err(format!("`text` is {} bytes; max {}", text.len(), MAX_BYTES));
                }
                let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut payload = text.as_bytes().to_vec();
                if enter {
                    payload.push(b'\r');
                }
                let pane_id = PaneId(pane_id_num);
                let session = sw.lock().get(&pane_id).cloned();
                let session =
                    session.ok_or_else(|| format!("no live PTY for pane {}", pane_id_num))?;
                session.write(&payload).map_err(|e| format!("write: {e}"))?;
                Ok(serde_json::json!({"bytes_written": payload.len()}))
            }),
        };
        register(cmds, cmd)?;
    }

    // Read the alacritty grid of any live PTY pane. `args = {pane_id: u64,
    // rows?: u16}`. Optional `rows` trims to the last N visible lines
    // (capped at 200 to avoid gigantic responses on scrollback-heavy panes).
    {
        let sw = session_writes.clone();
        let cmd = Command {
            id: "workspace.pane.output",
            title: "Read a pane's rendered output",
            description: Some("args: {pane_id: u64, rows?: u16<=200}"),
            run: Arc::new(move |args: &serde_json::Value| {
                const MAX_ROWS: u64 = 200;
                let pane_id_num = args
                    .get("pane_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "missing `pane_id` (u64)".to_string())?;
                let rows = match args.get("rows") {
                    Some(v) => Some(v.as_u64().ok_or_else(|| "`rows` must be u64".to_string())?),
                    None => None,
                };
                let rows = match rows {
                    Some(n) if n > MAX_ROWS => {
                        return Err(format!("rows {} > cap {}", n, MAX_ROWS));
                    }
                    Some(n) => Some(n as u16),
                    None => None,
                };
                let pane_id = PaneId(pane_id_num);
                let session = sw.lock().get(&pane_id).cloned();
                let session =
                    session.ok_or_else(|| format!("no live PTY for pane {}", pane_id_num))?;
                let contents = session.grid_contents(rows);
                let rows_captured = contents.lines().count();
                Ok(serde_json::json!({
                    "pane_id": pane_id_num,
                    "rows_captured": rows_captured,
                    "contents": contents,
                }))
            }),
        };
        register(cmds, cmd)?;
    }

    // Poll a pane's rendered output on the server side until `pattern`
    // (a Rust regex) matches or `timeout_ms` expires. Blocks the caller
    // synchronously; scripts avoid the sleep + poll dance.
    //
    //   args: {pane_id: u64, pattern: string, timeout_ms?: u64<=60000,
    //          poll_ms?: u64 in [25,1000]}
    //   → {pane_id, matched: bool, rows_captured, contents, elapsed_ms}
    {
        let sw = session_writes.clone();
        let cmd = Command {
            id: "workspace.pane.wait",
            title: "Wait until a regex matches a pane's output",
            description: Some("args: {pane_id, pattern, timeout_ms?<=60000, poll_ms?[25..=1000]}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let parsed = parse_wait_args(args)?;
                let pane_id = PaneId(parsed.pane_id);
                let session = sw
                    .lock()
                    .get(&pane_id)
                    .cloned()
                    .ok_or_else(|| format!("no live PTY for pane {}", parsed.pane_id))?;
                let start = std::time::Instant::now();
                let deadline = start + std::time::Duration::from_millis(parsed.timeout_ms);
                let poll = std::time::Duration::from_millis(parsed.poll_ms);
                loop {
                    let contents = session.grid_contents(Some(WAIT_READ_ROWS));
                    let matched = parsed.regex.is_match(&contents);
                    if matched || std::time::Instant::now() >= deadline {
                        let rows_captured = contents.lines().count();
                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        return Ok(serde_json::json!({
                            "pane_id": parsed.pane_id,
                            "matched": matched,
                            "rows_captured": rows_captured,
                            "contents": contents,
                            "elapsed_ms": elapsed_ms,
                        }));
                    }
                    // Don't oversleep past deadline: clamp to remaining.
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    std::thread::sleep(poll.min(remaining));
                }
            }),
        };
        register(cmds, cmd)?;
    }

    // Close whichever tab holds `pane_id`. Sync IPC entry: pushes a
    // `Close` mutation, blocks the caller until the main loop drains it.
    //
    //   args: {pane_id: u64}
    //   → {closed: true, pane_id}
    {
        let queue = pending_mutations.clone();
        let wake = redraw_tx.clone();
        let cmd = Command {
            id: "workspace.pane.close",
            title: "Close a pane by id",
            description: Some("args: {pane_id: u64}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let pane_id_num = parse_close_args(args)?;
                let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                queue.lock().push_back(PaneMutation::Close {
                    pane_id: PaneId(pane_id_num),
                    ack: ack_tx,
                });
                // Wake the main loop so the mutation drains promptly rather
                // than at the next input/redraw event.
                let _ = wake.send(());
                // 5s deadline covers the case where the app loop is wedged;
                // normal ticks resolve in single-digit ms.
                let outcome = ack_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| "app main loop dropped ack".to_string())?;
                outcome?;
                Ok(serde_json::json!({"closed": true, "pane_id": pane_id_num}))
            }),
        };
        register(cmds, cmd)?;
    }

    // v0.1 supports `kind = "shell"` (shells group) and `kind = "agent:<id>"`
    // where `<id>` ∈ AGENT_REGISTRY (agents group). Unknown kinds get a
    // 400 rather than a stub.
    //
    //   args: {kind: "shell" | "agent:<id>"}
    //   → {opened: true, pane_id, kind}
    {
        let queue = pending_mutations.clone();
        let wake = redraw_tx.clone();
        let cmd = Command {
            id: "workspace.pane.open",
            title: "Open a new pane of the given kind",
            description: Some("args: {kind: \"shell\" | \"agent:<id>\"}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let kind = parse_open_args(args)?;
                let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                let mutation = match kind {
                    OpenKind::Shell => PaneMutation::OpenShell { ack: ack_tx },
                    OpenKind::Agent(spec) => PaneMutation::OpenAgent { spec, ack: ack_tx },
                };
                queue.lock().push_back(mutation);
                let _ = wake.send(());
                let new_id = ack_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| "app main loop dropped ack".to_string())??;
                let kind_label = match kind {
                    OpenKind::Shell => "shell".to_string(),
                    OpenKind::Agent(spec) => format!("agent:{}", spec.id),
                };
                Ok(serde_json::json!({
                    "opened": true,
                    "pane_id": new_id,
                    "kind": kind_label,
                }))
            }),
        };
        register(cmds, cmd)?;
    }

    // Rename any pane in place. Scripts can use the fresh title as a
    // stable handle across snapshots ("build-runner" → find its pane_id
    // again next time). Title capped at RENAME_TITLE_MAX chars to keep
    // the tab strip readable.
    //
    //   args: {pane_id: u64, title: string (1..=64 chars, no `\n`)}
    //   → {renamed: true, pane_id, title}
    {
        let queue = pending_mutations.clone();
        let wake = redraw_tx.clone();
        let cmd = Command {
            id: "workspace.pane.rename",
            title: "Rename a pane by id",
            description: Some("args: {pane_id: u64, title: string (1..=64 chars)}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let (pane_id_num, title) = parse_rename_args(args)?;
                let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                queue.lock().push_back(PaneMutation::Rename {
                    pane_id: PaneId(pane_id_num),
                    title: title.clone(),
                    ack: ack_tx,
                });
                let _ = wake.send(());
                ack_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| "app main loop dropped ack".to_string())??;
                Ok(serde_json::json!({
                    "renamed": true,
                    "pane_id": pane_id_num,
                    "title": title,
                }))
            }),
        };
        register(cmds, cmd)?;
    }

    // Focus a pane. Activates its tab within the owning group and moves
    // the focus manager. Failing pane_id → 404, keyboard focus untouched.
    //
    //   args: {pane_id: u64}
    //   → {focused: true, pane_id}
    {
        let queue = pending_mutations.clone();
        let wake = redraw_tx.clone();
        let cmd = Command {
            id: "workspace.pane.focus",
            title: "Focus a pane by id",
            description: Some("args: {pane_id: u64}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let pane_id_num = parse_focus_args(args)?;
                let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                queue.lock().push_back(PaneMutation::Focus {
                    pane_id: PaneId(pane_id_num),
                    ack: ack_tx,
                });
                let _ = wake.send(());
                ack_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| "app main loop dropped ack".to_string())??;
                Ok(serde_json::json!({
                    "focused": true,
                    "pane_id": pane_id_num,
                }))
            }),
        };
        register(cmds, cmd)?;
    }

    // ── §9.4 Tools Registry ──
    //
    // `tools.list` → probe every entry in `TOOL_REGISTRY` and return
    // detected paths + install source. Pure `which` lookup, no mutation.
    {
        let cmd = Command {
            id: "tools.list",
            title: "List rimeterm-managed external tools",
            description: Some(
                "no args; returns detected path + install source for each of the five TUI tools",
            ),
            run: Arc::new(move |_args: &serde_json::Value| {
                let detected = rimeterm_config::tools::detect_all();
                Ok(serde_json::json!({ "tools": detected }))
            }),
        };
        register(cmds, cmd)?;
    }

    // `tools.install` / `.upgrade` / `.uninstall` all shell out to `cargo`.
    // v0.1: synchronous, capped at TOOL_ACTION_TIMEOUT_S; the tokio worker
    // that hosts this handler is blocked for the duration but the App loop
    // keeps ticking (multi-thread runtime). Streaming into a live pane is
    // C14+ (needs new pane kind).
    for (action_kind, ipc_id, ipc_title) in [
        (
            ToolAction::Install,
            "tools.install",
            "Install a tool via cargo install --locked",
        ),
        (
            ToolAction::Upgrade,
            "tools.upgrade",
            "Upgrade a tool via cargo install --locked --force",
        ),
        (
            ToolAction::Uninstall,
            "tools.uninstall",
            "Uninstall a cargo-installed tool",
        ),
    ] {
        let cmd = Command {
            id: ipc_id,
            title: ipc_title,
            description: Some("args: {name: string in registry}"),
            run: Arc::new(move |args: &serde_json::Value| {
                let name = parse_tool_action_args(args)?;
                let spec = rimeterm_config::tools::find(&name)
                    .ok_or_else(|| format!("unknown tool `{}`", name))?;
                run_tool_action(action_kind, spec)
            }),
        };
        register(cmds, cmd)?;
    }

    // ── §14 C14 Agents Picker ──
    //
    // `agents.list` mirrors `tools.list`: probes AGENT_REGISTRY, returns
    // detected paths + install hints so a script (or the Settings pane)
    // can render the picker itself.
    {
        let cmd = Command {
            id: "agents.list",
            title: "List detectable coding agents",
            description: Some("no args; returns per-agent detected path + install hint"),
            run: Arc::new(move |_args: &serde_json::Value| {
                let detected = rimeterm_pty::agent_registry::detect_all();
                Ok(serde_json::json!({ "agents": detected }))
            }),
        };
        register(cmds, cmd)?;
    }

    // `agents.pick.<id>` — one command per registry entry (static
    // literals so we don't need to synthesize `&'static str`s at
    // runtime, keeping both allocations and the `rs-box-leak` rule
    // happy). The macro takes only the agent id + label; it concats
    // the `agents.pick.` / `Open agent: ` prefixes internally so
    // adding a new agent is one line here + one row in
    // AGENT_REGISTRY.
    macro_rules! agent_pick_cmd {
        ($cmds:ident, $agent_id:literal, $label:literal) => {{
            let queue = pending_mutations.clone();
            let wake = redraw_tx.clone();
            let cmd = Command {
                id: concat!("agents.pick.", $agent_id),
                title: concat!("Open agent: ", $label),
                description: Some("spawn this agent in the agents quadrant"),
                run: Arc::new(move |_args: &serde_json::Value| {
                    // find() is guaranteed to return Some because the
                    // id is a compile-time literal that matches a
                    // registry entry (locked by the sixteen-agents
                    // test in rimeterm_pty::agent_registry).
                    let spec = rimeterm_pty::agent_registry::find($agent_id)
                        .expect("registry entry present");
                    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                    queue.lock().push_back(PaneMutation::OpenAgent {
                        spec,
                        ack: ack_tx,
                    });
                    let _ = wake.send(());
                    let new_id = ack_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .map_err(|_| "app main loop dropped ack".to_string())??;
                    Ok(serde_json::json!({
                        "opened": true,
                        "pane_id": new_id,
                        "kind": concat!("agent:", $agent_id),
                    }))
                }),
            };
            register($cmds, cmd)?;
        }};
    }
    // Order here matches AGENT_REGISTRY (alphabetical by label,
    // case-insensitive). Any drift is caught by the registry test +
    // the palette rendering (palette sorts by title anyway).
    agent_pick_cmd!(cmds, "antigravity", "Antigravity");
    agent_pick_cmd!(cmds, "claude", "Claude Code");
    agent_pick_cmd!(cmds, "codebuddy", "CodeBuddy");
    agent_pick_cmd!(cmds, "codex", "Codex");
    agent_pick_cmd!(cmds, "copilot", "Copilot");
    agent_pick_cmd!(cmds, "cursor", "Cursor");
    agent_pick_cmd!(cmds, "gemini", "Gemini CLI");
    agent_pick_cmd!(cmds, "hermes", "Hermes");
    agent_pick_cmd!(cmds, "kimi", "Kimi");
    agent_pick_cmd!(cmds, "kiro", "Kiro CLI");
    agent_pick_cmd!(cmds, "omp", "Oh-My-Pi");
    agent_pick_cmd!(cmds, "openclaw", "OpenClaw");
    agent_pick_cmd!(cmds, "opencode", "OpenCode");
    agent_pick_cmd!(cmds, "pi", "Pi");
    agent_pick_cmd!(cmds, "qoder", "Qoder");
    agent_pick_cmd!(cmds, "qwen", "Qwen Code");

    Ok(())
}

/// Read this many rows off the grid on every poll of
/// `workspace.pane.wait`. Chosen to cover even oversize terminals; the alacritty
/// grid usually holds ≤ 60 rows, so this is generous.
const WAIT_READ_ROWS: u16 = 200;
const WAIT_MAX_TIMEOUT_MS: u64 = 60_000;
const WAIT_MIN_POLL_MS: u64 = 25;
const WAIT_MAX_POLL_MS: u64 = 1000;
const WAIT_DEFAULT_POLL_MS: u64 = 100;
const WAIT_DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Validated inputs for `workspace.pane.wait`. Split from the closure body so
/// error paths (bad regex, missing pane_id, out-of-range poll) are unit-testable
/// without an App / PTY.
#[derive(Debug)]
pub(crate) struct WaitArgs {
    pub pane_id: u64,
    pub timeout_ms: u64,
    pub poll_ms: u64,
    pub regex: regex::Regex,
}

pub(crate) fn parse_wait_args(args: &serde_json::Value) -> Result<WaitArgs, String> {
    let pane_id = args
        .get("pane_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing `pane_id` (u64)".to_string())?;
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `pattern` (string)".to_string())?;
    let timeout_ms = match args.get("timeout_ms") {
        Some(v) => v
            .as_u64()
            .ok_or_else(|| "`timeout_ms` must be u64".to_string())?,
        None => WAIT_DEFAULT_TIMEOUT_MS,
    };
    if timeout_ms > WAIT_MAX_TIMEOUT_MS {
        return Err(format!(
            "timeout_ms {} > cap {}",
            timeout_ms, WAIT_MAX_TIMEOUT_MS
        ));
    }
    let poll_ms = match args.get("poll_ms") {
        Some(v) => v
            .as_u64()
            .ok_or_else(|| "`poll_ms` must be u64".to_string())?,
        None => WAIT_DEFAULT_POLL_MS,
    };
    if !(WAIT_MIN_POLL_MS..=WAIT_MAX_POLL_MS).contains(&poll_ms) {
        return Err(format!(
            "poll_ms {} outside [{},{}]",
            poll_ms, WAIT_MIN_POLL_MS, WAIT_MAX_POLL_MS
        ));
    }
    // Compile regex last so all cheap validation errors are reported first.
    let regex = regex::Regex::new(pattern).map_err(|e| format!("invalid regex: {e}"))?;
    Ok(WaitArgs {
        pane_id,
        timeout_ms,
        poll_ms,
        regex,
    })
}

/// Validated `pane_id` for `workspace.pane.close`. Split so error paths are
/// unit-testable without an App.
pub(crate) fn parse_close_args(args: &serde_json::Value) -> Result<u64, String> {
    args.get("pane_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing `pane_id` (u64)".to_string())
}

/// Structured kind for `workspace.pane.open`. `Shell` opens a new shell
/// in `BUILTIN_SHELLS`; `Agent(spec)` opens an agent PTY in
/// `BUILTIN_AGENTS`. The spec is looked up **before** returning so IPC
/// callers get an early "unknown agent" error.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OpenKind {
    Shell,
    Agent(&'static rimeterm_pty::agent_registry::AgentSpec),
}

/// Validated `kind` for `workspace.pane.open`. Accepts `"shell"` or
/// `"agent:<id>"` where `<id>` is a member of
/// [`rimeterm_pty::agent_registry::AGENT_REGISTRY`].
pub(crate) fn parse_open_args(args: &serde_json::Value) -> Result<OpenKind, String> {
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `kind` (string)".to_string())?;
    if kind == "shell" {
        return Ok(OpenKind::Shell);
    }
    if let Some(id) = kind.strip_prefix("agent:") {
        let spec = rimeterm_pty::agent_registry::find(id).ok_or_else(|| {
            format!(
                "unknown agent `{}`; try one of {:?}",
                id,
                rimeterm_pty::agent_registry::AGENT_REGISTRY
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
            )
        })?;
        return Ok(OpenKind::Agent(spec));
    }
    Err(format!(
        "kind `{}` not yet supported (try `shell` or `agent:<id>`)",
        kind
    ))
}

/// Cap on the human-visible tab title. Keeps the tab strip readable;
/// scripts wanting a longer handle should pick a shorter alias.
pub(crate) const RENAME_TITLE_MAX: usize = 64;

/// Validated `{pane_id, title}` for `workspace.pane.rename`. Rejects empty
/// strings, strings > `RENAME_TITLE_MAX` chars, and titles containing any
/// control character (`'\n'`, `'\r'`, `'\t'`, etc.) because those break
/// the tab strip.
pub(crate) fn parse_rename_args(args: &serde_json::Value) -> Result<(u64, String), String> {
    let pane_id = args
        .get("pane_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing `pane_id` (u64)".to_string())?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `title` (string)".to_string())?;
    if title.is_empty() {
        return Err("`title` must not be empty".to_string());
    }
    // char_indices lets us bail on the FIRST control char without scanning
    // the whole string; also gives us the byte offset for a decent error.
    if let Some((idx, ch)) = title.char_indices().find(|(_, c)| c.is_control()) {
        return Err(format!(
            "`title` contains control char {:?} at byte {}",
            ch, idx
        ));
    }
    if title.chars().count() > RENAME_TITLE_MAX {
        return Err(format!(
            "`title` too long ({} chars > cap {})",
            title.chars().count(),
            RENAME_TITLE_MAX
        ));
    }
    Ok((pane_id, title.to_string()))
}

/// Validated `pane_id` for `workspace.pane.focus`. Same shape as
/// `parse_close_args` but split so the error messages are distinct.
pub(crate) fn parse_focus_args(args: &serde_json::Value) -> Result<u64, String> {
    args.get("pane_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing `pane_id` (u64)".to_string())
}

/// Which cargo-side action a `tools.*` command triggers. Copy so the
/// per-registration closure below can capture it by value.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ToolAction {
    Install,
    Upgrade,
    Uninstall,
}

/// Cap on how long a single `cargo install` / `uninstall` may run before we
/// give up and let the IPC caller retry. 300 s covers a warm rebuild of the
/// biggest tool (yazi = ~200 crates); anything worse and the user needs to
/// run `cargo install` themselves.
pub(crate) const TOOL_ACTION_TIMEOUT_S: u64 = 300;

/// Validated `name` for `tools.install` / `tools.upgrade` / `tools.uninstall`.
/// Registry membership is checked by the caller — this only guarantees the
/// arg is a non-empty string.
pub(crate) fn parse_tool_action_args(args: &serde_json::Value) -> Result<String, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `name` (string)".to_string())?;
    if name.trim().is_empty() {
        return Err("`name` must not be empty".to_string());
    }
    Ok(name.to_string())
}

/// Shell out to `cargo` for a tool-registry entry. Blocks up to
/// `TOOL_ACTION_TIMEOUT_S`; returns exit status + captured stdout/stderr.
/// v0.1 blocks the current thread; C14 will pipe output into a live pane.
pub(crate) fn run_tool_action(
    action: ToolAction,
    spec: &'static rimeterm_config::tools::ToolSpec,
) -> Result<serde_json::Value, String> {
    // Refuse to shell out if `cargo` isn't on PATH — otherwise
    // `Command::spawn` prints a cryptic OS error and we look broken.
    if which::which("cargo").is_err() {
        return Err(
            "`cargo` not on PATH — install rustup (https://rustup.rs) before using the cargo install channel"
                .to_string(),
        );
    }

    // Build the argv per action.
    let mut argv: Vec<String> = Vec::new();
    let (action_label, uninstall_mode) = match action {
        ToolAction::Install => {
            argv.push("install".into());
            argv.push("--locked".into());
            for c in spec.crates {
                argv.push((*c).to_string());
            }
            ("install", false)
        }
        ToolAction::Upgrade => {
            argv.push("install".into());
            argv.push("--locked".into());
            argv.push("--force".into());
            for c in spec.crates {
                argv.push((*c).to_string());
            }
            ("upgrade", false)
        }
        ToolAction::Uninstall => {
            argv.push("uninstall".into());
            for c in spec.crates {
                argv.push((*c).to_string());
            }
            ("uninstall", true)
        }
    };

    // Uninstall only makes sense for cargo-installed binaries; if the
    // registry says the binary is system-managed, refuse before spending
    // cargo cycles on it.
    if uninstall_mode {
        let detected = rimeterm_config::tools::detect_one(
            spec,
            rimeterm_config::tools::cargo_bin_dir().as_deref(),
        );
        if detected.install_source == rimeterm_config::tools::InstallSource::System {
            return Err(format!(
                "`{}` is installed via a system package manager — uninstall it with the same tool",
                spec.name
            ));
        }
    }

    let start = std::time::Instant::now();
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(&argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn `cargo {}`: {}", action_label, e))?;

    // Poll with a hard deadline. `try_wait` is cheap; we sleep between
    // polls so we don't spin.
    let deadline = start + std::time::Duration::from_secs(TOOL_ACTION_TIMEOUT_S);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Collect the remaining output. The pipes stay open on
                // the child so we can just take the handles.
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr);
                }
                let elapsed_ms = start.elapsed().as_millis() as u64;
                return Ok(serde_json::json!({
                    "action": action_label,
                    "tool": spec.name,
                    "argv": argv,
                    "exit_code": status.code(),
                    "success": status.success(),
                    "elapsed_ms": elapsed_ms,
                    "stdout": stdout,
                    "stderr": stderr,
                }));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!(
                        "`cargo {}` exceeded {}s deadline; run it manually",
                        action_label, TOOL_ACTION_TIMEOUT_S
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("poll `cargo {}`: {}", action_label, e));
            }
        }
    }
}

fn tab_goto_title(index: usize) -> &'static str {
    match index {
        0 => "Go to tab 1",
        1 => "Go to tab 2",
        2 => "Go to tab 3",
        3 => "Go to tab 4",
        4 => "Go to tab 5",
        5 => "Go to tab 6",
        6 => "Go to tab 7",
        7 => "Go to tab 8",
        _ => "Go to tab 9",
    }
}

fn quadrant_title(idx: usize) -> &'static str {
    match idx {
        0 => "Focus files (top-left)",
        1 => "Focus agents (top-right)",
        2 => "Focus sysmon (bottom-left)",
        _ => "Focus shells (bottom-right)",
    }
}
/// Remove a pane from the registry; underlying Session's Drop closes the pty.
fn drop_pane(panes: &mut PaneRegistry, id: PaneId) {
    if let Some(boxed) = panes.remove(id) {
        drop(boxed);
    }
}

fn hint_bar_text() -> String {
    "Ctrl+Q Quit · F1 / Ctrl+Shift+P Palette · Alt+H/J/K/L Nav · Alt+1..4 Quadrant · Ctrl+PgUp/PgDn or Alt+[/] Tab · Ctrl+T new shell · F10 Menu".into()
}

fn point_in_rect(x: u16, y: u16, r: Rect) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

/// Return the [`HoveredDivider`] matching `(col, row)` inside `dividers`,
/// or `None` if the cursor is off every seam. Pure — no App state — so
/// it's cheap to call every `MouseEventKind::Moved` (fires roughly once
/// per pixel of mouse motion) and unit-testable without a live App.
pub(crate) fn find_hovered_divider(
    dividers: &[rimeterm_core::layout::Divider],
    col: u16,
    row: u16,
) -> Option<HoveredDivider> {
    dividers
        .iter()
        .find(|d| point_in_rect(col, row, d.visual.rect))
        .map(|d| HoveredDivider {
            path: d.path.clone(),
            boundary: d.boundary,
            axis: d.visual.axis,
            rect: d.visual.rect,
        })
}

/// Resolve the current-frame hover overlay: the seam rect + axis to
/// paint, or `None` to skip the overlay entirely.
///
/// Returns `None` when either:
/// - `dragging` is true (drag itself is the affordance; painting the
///   stale hover during drag pollutes cells the seam has already
///   moved away from), or
/// - the tracked hover key isn't in `dividers` anymore (rare — happens
///   right after a layout mutation like `workspace.layout.reset`).
///
/// Pure so we can unit-test the "no paint during drag" and "re-lookup
/// fresh rect" invariants without a live App / PTY.
pub(crate) fn live_hover_overlay(
    dragging: bool,
    hovered: Option<&HoveredDivider>,
    dividers: &[rimeterm_core::layout::Divider],
) -> Option<(Rect, ratatui::layout::Direction)> {
    if dragging {
        return None;
    }
    let hovered = hovered?;
    dividers
        .iter()
        .find(|d| d.path == hovered.path && d.boundary == hovered.boundary)
        .map(|d| (d.visual.rect, hovered.axis))
}

/// Parse the string form of a `TabGroupId` back to a static id. Called
/// from `run_context_intent` to decode `tab.close:shells:2` style tags.
fn parse_group_id(s: &str) -> Option<TabGroupId> {
    match s {
        "files" => Some(BUILTIN_FILES),
        "sysmon" => Some(BUILTIN_SYSMON),
        "agents" => Some(BUILTIN_AGENTS),
        "shells" => Some(BUILTIN_SHELLS),
        _ => None,
    }
}

/// Append the group-specific "new" entry (context menu builder helper).
/// Fixed groups get a disabled row explaining why.
fn push_group_new_entry(entries: &mut Vec<crate::picker::PickerEntry>, gid: TabGroupId) {
    if gid == BUILTIN_SHELLS {
        entries.push(crate::picker::PickerEntry::intent(
            "New shell tab",
            "shells.new",
        ));
    } else if gid == BUILTIN_AGENTS {
        entries.push(crate::picker::PickerEntry::intent(
            "Open agent picker…",
            "agents.pick",
        ));
    } else {
        entries.push(crate::picker::PickerEntry::disabled(
            "New tab",
            "(fixed group)",
        ));
    }
}

/// Compute the rect the *parent split* at `path` occupies inside `pane_area`.
/// Walks the tree the same way the layout walker does, applying each step's
/// child index and re-splitting the parent by its ratios.
fn split_parent_rect(
    tree: &LayoutTree,
    pane_area: Rect,
    path: &rimeterm_core::layout::SplitPath,
) -> Option<Rect> {
    let mut node = tree.root();
    let mut area = pane_area;
    for &step in &path.0 {
        let (direction, ratios, children) = match node {
            LayoutNode::Split {
                direction,
                ratios,
                children,
            } => (direction, ratios, children),
            _ => return None,
        };
        let constraints: Vec<Constraint> = ratios
            .iter()
            .map(|r| Constraint::Ratio((*r * 10_000.0).round() as u32, 10_000))
            .collect();
        let rects = Layout::default()
            .direction(*direction)
            .constraints(constraints)
            .split(area);
        area = *rects.get(step as usize)?;
        node = children.get(step as usize)?;
    }
    // node is now the split we care about; its rect IS `area`.
    matches!(node, LayoutNode::Split { .. }).then_some(area)
}

/// Compute the min-size floor (as a fraction of parent extent) per child of
/// the split at `path`. v0.1 hardcodes the design-doc §19.8 defaults; a later
/// milestone reads them from config.
fn min_size_floors(
    tree: &LayoutTree,
    path: &rimeterm_core::layout::SplitPath,
    parent_extent: u16,
) -> Vec<f32> {
    // §19.8 defaults: yazi/sysmon 24 cols · agents/shells 32 cols · viewer 48
    //                 rows: 6 (sysmon/shells) · 8 (yazi) · 10 (agents) · 12 (viewer)
    let floors_cells: [u16; 2] = match path.0.as_slice() {
        // Root split: two columns. Left = files/sysmon (24), right = agents/shells (32).
        [] => [24, 32],
        // Left column vertical split: files above sysmon.
        [0] => [8, 6],
        // Right column vertical split: agents above shells.
        [1] => [10, 6],
        _ => [1, 1],
    };
    let _ = tree;
    if parent_extent == 0 {
        return vec![0.0; floors_cells.len()];
    }
    floors_cells
        .iter()
        .map(|c| (*c as f32) / (parent_extent as f32))
        .collect()
}

/// Take a snapshot of every split's ratios keyed by path so we can `= / 0` reset.
fn snapshot_all_ratios(tree: &LayoutTree) -> Vec<(rimeterm_core::layout::SplitPath, Vec<f32>)> {
    let mut out = Vec::new();
    walk_snapshot(
        tree.root(),
        rimeterm_core::layout::SplitPath::root(),
        &mut out,
    );
    out
}

fn walk_snapshot(
    node: &LayoutNode,
    path: rimeterm_core::layout::SplitPath,
    out: &mut Vec<(rimeterm_core::layout::SplitPath, Vec<f32>)>,
) {
    if let LayoutNode::Split {
        ratios, children, ..
    } = node
    {
        out.push((path.clone(), ratios.clone()));
        for (idx, child) in children.iter().enumerate() {
            walk_snapshot(child, path.clone().push(idx as u8), out);
        }
    }
}

/// Overwrite the tree's ratios from a persisted [`LayoutState`]. Missing paths
/// keep their defaults; unknown paths in the state file are silently skipped.
fn apply_persisted_state(
    tree: &mut LayoutTree,
    state: &rimeterm_config::layout_state::LayoutState,
) {
    for (key, ratios) in &state.splits {
        let path = rimeterm_core::layout::SplitPath(
            rimeterm_config::layout_state::LayoutState::decode_path(key),
        );
        let _ = tree.set_ratios(&path, ratios.clone());
    }
}

/// Snapshot the tree's current ratios into a persistable [`LayoutState`].
fn snapshot_persisted_state(tree: &LayoutTree) -> rimeterm_config::layout_state::LayoutState {
    let mut state = rimeterm_config::layout_state::LayoutState::default();
    for (path, ratios) in snapshot_all_ratios(tree) {
        let key = rimeterm_config::layout_state::LayoutState::encode_path(&path.0);
        state.splits.insert(key, ratios);
    }
    state
}

fn next_shell_number(members: &[PaneId], panes: &PaneRegistry) -> usize {
    let mut max = 0usize;
    for id in members {
        if let Some(pane) = panes.get(*id) {
            if let Some(n) = pane
                .title()
                .strip_prefix("shell-")
                .and_then(|s| s.parse::<usize>().ok())
            {
                if n > max {
                    max = n;
                }
            }
        }
    }
    max + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbor_group_navigates_left_right() {
        assert_eq!(neighbor_group(BUILTIN_FILES, 2), Some(BUILTIN_AGENTS));
        assert_eq!(neighbor_group(BUILTIN_AGENTS, 1), Some(BUILTIN_FILES));
        assert_eq!(neighbor_group(BUILTIN_SYSMON, 2), Some(BUILTIN_SHELLS));
        assert_eq!(neighbor_group(BUILTIN_SHELLS, 1), Some(BUILTIN_SYSMON));
    }

    #[test]
    fn neighbor_group_navigates_up_down() {
        assert_eq!(neighbor_group(BUILTIN_FILES, 4), Some(BUILTIN_SYSMON));
        assert_eq!(neighbor_group(BUILTIN_SYSMON, 3), Some(BUILTIN_FILES));
        assert_eq!(neighbor_group(BUILTIN_AGENTS, 4), Some(BUILTIN_SHELLS));
        assert_eq!(neighbor_group(BUILTIN_SHELLS, 3), Some(BUILTIN_AGENTS));
    }

    #[test]
    fn neighbor_group_returns_none_when_no_neighbor() {
        // Going up from a top-row group has no neighbor.
        assert_eq!(neighbor_group(BUILTIN_FILES, 3), None);
        assert_eq!(neighbor_group(BUILTIN_AGENTS, 3), None);
        // Going down from a bottom-row group has no neighbor.
        assert_eq!(neighbor_group(BUILTIN_SYSMON, 4), None);
        assert_eq!(neighbor_group(BUILTIN_SHELLS, 4), None);
        // Going left from the left column has no neighbor.
        assert_eq!(neighbor_group(BUILTIN_FILES, 1), None);
        assert_eq!(neighbor_group(BUILTIN_SYSMON, 1), None);
        // Going right from the right column has no neighbor.
        assert_eq!(neighbor_group(BUILTIN_AGENTS, 2), None);
        assert_eq!(neighbor_group(BUILTIN_SHELLS, 2), None);
    }

    #[test]
    fn resize_target_maps_group_to_split_path() {
        use rimeterm_core::layout::SplitPath;
        let (path, boundary, _, sign) =
            resize_target_for_group(BUILTIN_FILES, ResizeTarget::Horizontal).unwrap();
        assert_eq!(path, SplitPath::root());
        assert_eq!(boundary, 0);
        assert!(sign > 0.0);

        let (path, _, _, sign) =
            resize_target_for_group(BUILTIN_AGENTS, ResizeTarget::Horizontal).unwrap();
        assert_eq!(path, SplitPath::root());
        assert!(sign < 0.0);

        let (path, _, _, sign) =
            resize_target_for_group(BUILTIN_FILES, ResizeTarget::Vertical).unwrap();
        assert_eq!(path, SplitPath::root().push(0));
        assert!(sign > 0.0);

        let (path, _, _, sign) =
            resize_target_for_group(BUILTIN_SHELLS, ResizeTarget::Vertical).unwrap();
        assert_eq!(path, SplitPath::root().push(1));
        assert!(sign < 0.0);
    }

    #[test]
    fn paths_for_group_returns_column_split_and_root() {
        use rimeterm_core::layout::SplitPath;
        assert_eq!(
            paths_for_group(BUILTIN_AGENTS),
            vec![SplitPath::root(), SplitPath::root().push(1)]
        );
        assert_eq!(
            paths_for_group(BUILTIN_FILES),
            vec![SplitPath::root(), SplitPath::root().push(0)]
        );
    }

    #[test]
    fn simulated_drag_moves_root_seam_within_floor() {
        // Build a mini tree matching the app's shape: horizontal 0.35 / 0.65.
        use rimeterm_core::layout::{LayoutNode, LayoutTree, SplitPath};
        use rimeterm_core::pane::PaneId;
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![0.35, 0.65],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        // 100-cell wide parent; drag right by 10 cells → delta ratio +0.10.
        let floors = min_size_floors(&tree, &SplitPath::root(), 100);
        tree.adjust_ratio(&SplitPath::root(), 0, 0.10, &floors)
            .unwrap();
        let ratios = tree.ratios_at(&SplitPath::root()).unwrap();
        assert!((ratios[0] - 0.45).abs() < 1e-6);
        assert!((ratios[1] - 0.55).abs() < 1e-6);
    }

    #[test]
    fn drag_rejected_at_min_size_floor() {
        use rimeterm_core::layout::{LayoutNode, LayoutTree, RatioError, SplitPath};
        use rimeterm_core::pane::PaneId;
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![0.35, 0.65],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        // 100-cell parent, floors [24, 32] → left floor 0.24, right floor 0.32.
        // Try to drag left by 20 cells → left would become 0.15 → reject.
        let floors = min_size_floors(&tree, &SplitPath::root(), 100);
        let err = tree
            .adjust_ratio(&SplitPath::root(), 0, -0.20, &floors)
            .unwrap_err();
        assert_eq!(err, RatioError::BelowMinSize);
    }

    // --- workspace.pane.wait input validation ---

    fn wait_args_json(spec: &str) -> serde_json::Value {
        serde_json::from_str(spec).expect("test spec must be valid JSON")
    }

    #[test]
    fn wait_args_reject_missing_pane_id() {
        let err = parse_wait_args(&wait_args_json(r#"{"pattern":"foo"}"#)).unwrap_err();
        assert!(err.contains("pane_id"), "err was {err:?}");
    }

    #[test]
    fn wait_args_reject_missing_pattern() {
        let err = parse_wait_args(&wait_args_json(r#"{"pane_id":10}"#)).unwrap_err();
        assert!(err.contains("pattern"), "err was {err:?}");
    }

    #[test]
    fn wait_args_reject_bad_regex() {
        let err = parse_wait_args(&wait_args_json(r#"{"pane_id":10,"pattern":"("}"#)).unwrap_err();
        assert!(err.contains("invalid regex"), "err was {err:?}");
    }

    #[test]
    fn wait_args_reject_timeout_over_cap() {
        let err = parse_wait_args(&wait_args_json(
            r#"{"pane_id":10,"pattern":"foo","timeout_ms":120000}"#,
        ))
        .unwrap_err();
        assert!(err.contains("timeout_ms"), "err was {err:?}");
        assert!(err.contains("cap"), "err was {err:?}");
    }

    #[test]
    fn wait_args_reject_poll_out_of_range() {
        let err = parse_wait_args(&wait_args_json(
            r#"{"pane_id":10,"pattern":"foo","poll_ms":5}"#,
        ))
        .unwrap_err();
        assert!(err.contains("poll_ms"), "err was {err:?}");
    }

    #[test]
    fn wait_args_defaults_when_omitted() {
        let args =
            parse_wait_args(&wait_args_json(r#"{"pane_id":10,"pattern":"^done$"}"#)).unwrap();
        assert_eq!(args.pane_id, 10);
        assert_eq!(args.timeout_ms, WAIT_DEFAULT_TIMEOUT_MS);
        assert_eq!(args.poll_ms, WAIT_DEFAULT_POLL_MS);
        assert!(args.regex.is_match("done"));
        assert!(!args.regex.is_match("not done here"));
    }

    #[test]
    fn wait_args_accepts_boundary_values() {
        let args = parse_wait_args(&wait_args_json(
            r#"{"pane_id":7,"pattern":"x","timeout_ms":60000,"poll_ms":25}"#,
        ))
        .unwrap();
        assert_eq!(args.timeout_ms, WAIT_MAX_TIMEOUT_MS);
        assert_eq!(args.poll_ms, WAIT_MIN_POLL_MS);
    }

    // --- workspace.pane.close input validation ---

    #[test]
    fn close_args_reject_missing_pane_id() {
        let err = parse_close_args(&wait_args_json(r#"{}"#)).unwrap_err();
        assert!(err.contains("pane_id"), "err was {err:?}");
    }

    #[test]
    fn close_args_reject_wrong_type() {
        let err = parse_close_args(&wait_args_json(r#"{"pane_id":"nope"}"#)).unwrap_err();
        assert!(err.contains("pane_id"), "err was {err:?}");
    }

    #[test]
    fn close_args_accept_valid_u64() {
        let id = parse_close_args(&wait_args_json(r#"{"pane_id": 42}"#)).unwrap();
        assert_eq!(id, 42);
    }

    // --- workspace.pane.open input validation ---

    #[test]
    fn open_args_reject_missing_kind() {
        let err = parse_open_args(&wait_args_json(r#"{}"#)).unwrap_err();
        assert!(err.contains("kind"), "err was {err:?}");
    }

    #[test]
    fn open_args_reject_bare_agent() {
        // "agent" alone (without ":<id>") is not a valid kind.
        let err = parse_open_args(&wait_args_json(r#"{"kind":"agent"}"#)).unwrap_err();
        assert!(err.contains("agent"), "err was {err:?}");
        assert!(err.contains("try `shell`"), "err was {err:?}");
    }

    #[test]
    fn open_args_accept_shell_kind() {
        let kind = parse_open_args(&wait_args_json(r#"{"kind":"shell"}"#)).unwrap();
        assert!(matches!(kind, OpenKind::Shell));
    }

    #[test]
    fn open_args_accept_registered_agent() {
        let kind = parse_open_args(&wait_args_json(r#"{"kind":"agent:codex"}"#)).unwrap();
        match kind {
            OpenKind::Agent(spec) => assert_eq!(spec.id, "codex"),
            OpenKind::Shell => panic!("expected agent kind"),
        }
    }

    #[test]
    fn open_args_reject_unknown_agent() {
        let err = parse_open_args(&wait_args_json(r#"{"kind":"agent:nope"}"#)).unwrap_err();
        assert!(err.contains("unknown agent"), "err was {err:?}");
    }

    // --- workspace.pane.rename input validation ---

    #[test]
    fn rename_args_reject_missing_pane_id() {
        let err = parse_rename_args(&wait_args_json(r#"{"title":"x"}"#)).unwrap_err();
        assert!(err.contains("pane_id"), "err was {err:?}");
    }

    #[test]
    fn rename_args_reject_missing_title() {
        let err = parse_rename_args(&wait_args_json(r#"{"pane_id":10}"#)).unwrap_err();
        assert!(err.contains("title"), "err was {err:?}");
    }

    #[test]
    fn rename_args_reject_empty_title() {
        let err = parse_rename_args(&wait_args_json(r#"{"pane_id":10,"title":""}"#)).unwrap_err();
        assert!(err.contains("empty"), "err was {err:?}");
    }

    #[test]
    fn rename_args_reject_control_char() {
        let err =
            parse_rename_args(&wait_args_json(r#"{"pane_id":10,"title":"foo\nbar"}"#)).unwrap_err();
        assert!(err.contains("control char"), "err was {err:?}");
    }

    #[test]
    fn rename_args_reject_title_over_cap() {
        // 65 chars > cap 64. Use serde_json to build the payload so we don't
        // have to hand-count escapes.
        let long = "x".repeat(RENAME_TITLE_MAX + 1);
        let payload = serde_json::json!({"pane_id": 10, "title": long});
        let err = parse_rename_args(&payload).unwrap_err();
        assert!(err.contains("too long"), "err was {err:?}");
    }

    #[test]
    fn rename_args_accept_boundary_length() {
        let title = "x".repeat(RENAME_TITLE_MAX);
        let payload = serde_json::json!({"pane_id": 7, "title": title.clone()});
        let (pane_id, out_title) = parse_rename_args(&payload).unwrap();
        assert_eq!(pane_id, 7);
        assert_eq!(out_title, title);
    }

    #[test]
    fn rename_args_accept_unicode() {
        let (_, title) =
            parse_rename_args(&wait_args_json(r#"{"pane_id": 3, "title": "构建-runner"}"#))
                .unwrap();
        assert_eq!(title, "构建-runner");
    }

    // --- workspace.pane.focus input validation ---

    #[test]
    fn focus_args_reject_missing_pane_id() {
        let err = parse_focus_args(&wait_args_json(r#"{}"#)).unwrap_err();
        assert!(err.contains("pane_id"), "err was {err:?}");
    }

    #[test]
    fn focus_args_accept_valid_u64() {
        let id = parse_focus_args(&wait_args_json(r#"{"pane_id": 99}"#)).unwrap();
        assert_eq!(id, 99);
    }

    // --- tools.install / tools.upgrade / tools.uninstall input validation ---

    #[test]
    fn tool_action_args_reject_missing_name() {
        let err = parse_tool_action_args(&wait_args_json(r#"{}"#)).unwrap_err();
        assert!(err.contains("name"), "err was {err:?}");
    }

    #[test]
    fn tool_action_args_reject_empty_name() {
        let err = parse_tool_action_args(&wait_args_json(r#"{"name":"   "}"#)).unwrap_err();
        assert!(err.contains("empty"), "err was {err:?}");
    }

    #[test]
    fn tool_action_args_accept_valid_name() {
        let name = parse_tool_action_args(&wait_args_json(r#"{"name":"gitui"}"#)).unwrap();
        assert_eq!(name, "gitui");
    }

    // --- find_hovered_divider (C16 hover tracking) ---

    fn mk_divider(
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        axis: Direction,
    ) -> rimeterm_core::layout::Divider {
        rimeterm_core::layout::Divider {
            path: rimeterm_core::layout::SplitPath::root(),
            boundary: 0,
            visual: rimeterm_core::layout::DividerRect {
                axis,
                rect: Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
            },
        }
    }

    #[test]
    fn hovered_divider_none_when_no_dividers() {
        assert!(find_hovered_divider(&[], 10, 10).is_none());
    }

    #[test]
    fn hovered_divider_none_when_outside_all_rects() {
        // Vertical seam at x=20, rows 5..15.
        let d = mk_divider(20, 5, 1, 10, Direction::Horizontal);
        assert!(
            find_hovered_divider(&[d.clone()], 19, 5).is_none(),
            "just left"
        );
        assert!(
            find_hovered_divider(&[d.clone()], 21, 5).is_none(),
            "just right"
        );
        assert!(find_hovered_divider(&[d.clone()], 20, 4).is_none(), "above");
        assert!(
            find_hovered_divider(&[d], 20, 15).is_none(),
            "below (row 15 is exclusive)"
        );
    }

    #[test]
    fn hovered_divider_matches_when_inside_rect() {
        let d = mk_divider(20, 5, 1, 10, Direction::Horizontal);
        let h = find_hovered_divider(&[d.clone()], 20, 10).expect("should hit");
        // Same key + axis + rect.
        assert_eq!(h.axis, Direction::Horizontal);
        assert_eq!(h.rect, d.visual.rect);
        assert_eq!(h.boundary, 0);
    }

    #[test]
    fn hovered_divider_picks_first_when_rects_overlap() {
        // Two dividers at the same rect — first-match wins. Real layouts
        // never produce overlapping seams (dividers live in disjoint
        // splits), but the function must still be deterministic.
        let a = mk_divider(10, 5, 1, 10, Direction::Horizontal);
        let b = mk_divider(10, 5, 1, 10, Direction::Vertical);
        let h = find_hovered_divider(&[a.clone(), b], 10, 7).unwrap();
        assert_eq!(h.axis, Direction::Horizontal);
    }

    #[test]
    fn hovered_divider_axis_reports_split_direction() {
        // Horizontal split → vertical seam (side-by-side panes). Vertical
        // split → horizontal seam (stacked panes). The axis on
        // HoveredDivider is the parent split direction, which drives the
        // hint-bar glyph (↔ vs ↕) in draw().
        let hz = mk_divider(50, 0, 1, 20, Direction::Horizontal);
        assert_eq!(
            find_hovered_divider(&[hz], 50, 10).unwrap().axis,
            Direction::Horizontal
        );
        let vt = mk_divider(0, 10, 100, 1, Direction::Vertical);
        assert_eq!(
            find_hovered_divider(&[vt], 50, 10).unwrap().axis,
            Direction::Vertical
        );
    }

    // --- live_hover_overlay (drag-safety + freshness) ---

    fn mk_hovered(x: u16, y: u16, w: u16, h: u16, axis: Direction) -> HoveredDivider {
        HoveredDivider {
            path: rimeterm_core::layout::SplitPath::root(),
            boundary: 0,
            axis,
            rect: Rect {
                x,
                y,
                width: w,
                height: h,
            },
        }
    }

    #[test]
    fn overlay_none_when_dragging_even_with_matching_hover() {
        // The dragging guard exists precisely because the cached hover
        // rect is stale mid-drag (seam has moved, but hover state
        // hasn't been refreshed yet). Returning `None` here suppresses
        // the yellow-pollution bug where the pre-drag seam cells stay
        // painted while the actual seam has slid elsewhere.
        let d = mk_divider(20, 5, 1, 10, Direction::Horizontal);
        let h = mk_hovered(20, 5, 1, 10, Direction::Horizontal);
        assert!(live_hover_overlay(true, Some(&h), &[d]).is_none());
    }

    #[test]
    fn overlay_none_when_no_hover_tracked() {
        // Nothing hovered → nothing to paint. Trivial but locks in the
        // early-return path.
        let d = mk_divider(20, 5, 1, 10, Direction::Horizontal);
        assert!(live_hover_overlay(false, None, &[d]).is_none());
    }

    #[test]
    fn overlay_uses_fresh_rect_from_dividers_not_cached_snapshot() {
        // Simulate the "ratios changed between Moved and next frame"
        // case: hover cache still says (20,5), but the live divider is
        // now at (30,5). Overlay must paint (30,5), not the stale
        // (20,5) — otherwise the yellow highlight would trail the
        // actual seam and pollute normal pane cells.
        let stale_hover = mk_hovered(20, 5, 1, 10, Direction::Horizontal);
        let live_divider = mk_divider(30, 5, 1, 10, Direction::Horizontal);
        let (rect, axis) = live_hover_overlay(false, Some(&stale_hover), &[live_divider]).unwrap();
        assert_eq!(rect.x, 30, "should read live divider's x, not cached 20");
        assert_eq!(axis, Direction::Horizontal);
    }

    #[test]
    fn overlay_none_when_hovered_divider_disappeared() {
        // If a layout mutation dropped the divider (e.g.
        // `workspace.layout.reset` restructured the tree), the hover
        // key won't match any current divider. Bail — safer than
        // painting whatever the first divider happens to be.
        let stale_hover = mk_hovered(20, 5, 1, 10, Direction::Horizontal);
        let unrelated = rimeterm_core::layout::Divider {
            path: rimeterm_core::layout::SplitPath::root().push(0),
            boundary: 0,
            visual: rimeterm_core::layout::DividerRect {
                axis: Direction::Vertical,
                rect: Rect {
                    x: 0,
                    y: 15,
                    width: 40,
                    height: 1,
                },
            },
        };
        assert!(live_hover_overlay(false, Some(&stale_hover), &[unrelated]).is_none());
    }

    // --- spinner_glyph (spawn-progress animation) ---

    #[test]
    fn spinner_cycles_every_100ms() {
        use std::time::Duration;
        assert_eq!(spinner_glyph(Duration::from_millis(0)), SPINNER_FRAMES[0]);
        assert_eq!(spinner_glyph(Duration::from_millis(99)), SPINNER_FRAMES[0]);
        assert_eq!(spinner_glyph(Duration::from_millis(100)), SPINNER_FRAMES[1]);
        assert_eq!(spinner_glyph(Duration::from_millis(200)), SPINNER_FRAMES[2]);
        assert_eq!(spinner_glyph(Duration::from_millis(700)), SPINNER_FRAMES[7]);
    }

    #[test]
    fn spinner_wraps_at_frame_count() {
        use std::time::Duration;
        // 8 frames × 100ms = one full cycle at 800ms.
        assert_eq!(spinner_glyph(Duration::from_millis(800)), SPINNER_FRAMES[0]);
        assert_eq!(spinner_glyph(Duration::from_millis(900)), SPINNER_FRAMES[1]);
        // Longer waits still yield a valid glyph — modulo, no panic.
        assert_eq!(
            spinner_glyph(Duration::from_secs(60)),
            SPINNER_FRAMES[(60_000 / 100) % SPINNER_FRAMES.len()]
        );
    }

    #[test]
    fn spinner_frames_are_all_single_grapheme_and_nonempty() {
        // Locks the invariant the render path depends on: hint bar
        // width math assumes one displayable column per frame. If a
        // future change slips a multi-char string in here, the width
        // clamp in the paragraph render would misalign.
        for f in SPINNER_FRAMES {
            assert!(!f.is_empty(), "empty spinner frame");
            assert_eq!(
                f.chars().count(),
                1,
                "spinner frame `{}` must be a single scalar",
                f
            );
        }
    }

    // --- pending_spawn_should_clear (spawn-progress classification) ---

    #[test]
    fn spawn_clears_when_pane_vanished() {
        // Session dropped from session_writes → caller passes None.
        // Must clear so the spinner doesn't outlive its pane.
        assert!(pending_spawn_should_clear(
            std::time::Duration::from_secs(1),
            None
        ));
    }

    #[test]
    fn spawn_clears_on_timeout_even_without_output() {
        // Pane still exists (grid = ""), but boot deadline hit — stop nagging.
        assert!(pending_spawn_should_clear(
            PENDING_SPAWN_TIMEOUT,
            Some("")
        ));
        assert!(pending_spawn_should_clear(
            PENDING_SPAWN_TIMEOUT + std::time::Duration::from_secs(1),
            Some("            \n\n\n")
        ));
    }

    #[test]
    fn spawn_keeps_spinning_before_deadline_and_no_output() {
        // Fresh spawn + blank grid = the exact state the spinner exists for.
        assert!(!pending_spawn_should_clear(
            std::time::Duration::from_millis(500),
            Some("")
        ));
        // Whitespace-only grids (rendered blank rows / cursor at origin
        // over an empty terminal) MUST NOT count as output — that's the
        // false-positive we're guarding against.
        assert!(!pending_spawn_should_clear(
            std::time::Duration::from_millis(500),
            Some("   \n   \n   \n")
        ));
    }

    #[test]
    fn spawn_clears_on_any_nonwhitespace_char() {
        // Regression: the old tail-only sample missed banners at the top
        // of alt-screen TUIs. Content anywhere in the sample must clear.
        assert!(pending_spawn_should_clear(
            std::time::Duration::from_millis(200),
            // Banner at top, blank tail — the exact shape of a fresh
            // claude / codex / omp launch.
            Some("Welcome to omp!\n\n\n\n\n\n\n")
        ));
        assert!(pending_spawn_should_clear(
            std::time::Duration::from_millis(200),
            Some("$ ")
        ));
    }
}
