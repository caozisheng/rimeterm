//! Shared types for the workspace tab region of the single-row top
//! bar (see [`crate::top_bar`], which owns the rendering + geometry).
//!
//! What remains here: the hit/hover vocabulary the app's mouse router
//! uses, the double-click streak detector that opens the rename
//! editor, and the rename editor state itself.

/// Which workspace-tab affordance the pointer is over right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceHover {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    New,
    /// Hover the overflow affordance (`‹` / `›`) that scrolls hidden
    /// workspace tabs into view.
    OverflowPrev,
    OverflowNext,
}

/// A hit-test result for the workspace tab region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceHit {
    /// Click activates workspace `usize`.
    Tab(usize),
    /// Click closes workspace `usize` (last tab routes through the exit
    /// dialog, see `App::close_workspace`).
    Close(usize),
    /// Click duplicates the active workspace (`[+]` affordance).
    New,
    /// Click scrolls hidden tabs into view (`‹` / `›`).
    OverflowPrev,
    OverflowNext,
}

/// Double-click window for opening the rename editor. Matches
/// [`crate::pty_selection::MULTI_CLICK_MS`] / xterm's default.
const DOUBLE_CLICK_MS: u128 = 400;

/// Double-click streak detector that decides when a Down on a tab
/// opens the rename editor. `App` owns one permanently (streaks must
/// survive across clicks even when no editor is open); see
/// [`crate::pty_selection`] for the PTY-side equivalent.
#[derive(Debug, Clone)]
pub struct WorkspaceClickStreak {
    last_click: (u16, u16),
    last_click_at: std::time::Instant,
}

impl Default for WorkspaceClickStreak {
    fn default() -> Self {
        Self {
            last_click: (u16::MAX, u16::MAX),
            last_click_at: std::time::Instant::now(),
        }
    }
}

impl WorkspaceClickStreak {
    /// Register a fresh `Down` at the given cell. Returns `true` when
    /// it lands on the same cell as the previous Down within
    /// [`DOUBLE_CLICK_MS`], i.e. the caller should treat it as the
    /// second click of a double-click.
    pub fn register(&mut self, col: u16, row: u16, now: std::time::Instant) -> bool {
        let doubled = self.last_click == (col, row)
            && now.duration_since(self.last_click_at).as_millis() <= DOUBLE_CLICK_MS;
        self.last_click = (col, row);
        self.last_click_at = now;
        doubled
    }
}

/// In-progress rename of a workspace tab. `App` owns this; the top
/// bar renders the edit pill while it is `Some`.
///
/// Keys while open (see `App::on_key`): typing appends, Backspace
/// deletes, Enter accepts (empty restores the auto title), Esc
/// cancels. A mouse Down anywhere else commits (browser inline-edit
/// blur behavior).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRename {
    /// Logical workspace index being renamed.
    pub index: usize,
    /// Draft text.
    pub buffer: String,
}
