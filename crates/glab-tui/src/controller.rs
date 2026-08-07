//! Pure event→outcome reducer for glab-tui.
//!
//! Every key or mouse event enters through [`handle_key`] or [`handle_mouse`]
//! and returns a [`ControllerOutcome`] that the host loop can interpret without
//! the controller touching the terminal or spawning processes itself.

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::App;
use crate::event::Event;

// ---------------------------------------------------------------------------
// Outcome types
// ---------------------------------------------------------------------------

/// What the host loop should do after a key/mouse event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerOutcome {
    /// Nothing changed; the host may skip a redraw.
    Unchanged,
    /// App state mutated; the host should redraw.
    Changed,
    /// The user requested to quit the application.
    ExitRequested,
    /// The controller wants to run an external command.
    Command(CommandIntent),
    /// The controller needs the host to perform a side-effect that
    /// requires terminal ownership (e.g. spawning an editor).
    HostAction(HostAction),
}

/// An intent to run a command — the host decides *how*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandIntent {
    /// Run a CLI command and feed the result back through the event channel.
    Cli { program: String, args: Vec<String> },
}

/// Side-effects that require terminal ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    /// Open an external editor with the given text content.
    /// `suffix` is the temp-file extension (e.g. ".md", ".txt").
    /// The host should write `content` to a temp file, leave raw mode,
    /// spawn the editor, then return the edited content through the
    /// event channel.
    EditText { content: String, suffix: String },
    /// Open a URL in the system browser.
    /// The host should call the platform opener (e.g. `open`, `xdg-open`,
    /// `start`) and report completion via [`HostActionResult::OpenUrlCompleted`].
    OpenUrl(String),
    /// Copy text to the system clipboard.
    /// The host should interact with the OS clipboard and report completion
    /// via [`HostActionResult::CopyCompleted`].
    CopyText(String),
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Top-level key dispatcher. Calls into the handler modules and translates
/// their results into a [`ControllerOutcome`].
pub fn handle_key(
    app: &mut App,
    key: KeyEvent,
    tx: UnboundedSender<Event>,
    last_refresh: &mut Instant,
) -> ControllerOutcome {
    // Overlay handlers run first — they swallow keys when an overlay is active.
    if crate::handlers::overlays::handle_confirm_popup(app, &key, tx.clone()) {
        return ControllerOutcome::Changed;
    }
    if crate::handlers::overlays::handle_help_overlay(app, &key) {
        return ControllerOutcome::Changed;
    }
    if crate::handlers::overlays::handle_date_picker(app, &key, tx.clone()) {
        return ControllerOutcome::Changed;
    }
    if crate::handlers::overlays::handle_help_keybinding(app, &key) {
        return ControllerOutcome::Changed;
    }
    if crate::handlers::overlays::handle_switch_repo(app, &key) {
        return ControllerOutcome::Changed;
    }
    if crate::handlers::overlays::handle_refresh(app, &key, last_refresh, tx.clone()) {
        return ControllerOutcome::Changed;
    }

    // Tab-specific key handling is async; we cannot call it directly from a
    // sync fn.  The host loop should call `handle_active_tab_key` itself
    // when none of the overlay handlers consumed the key.
    //
    // For now return Unchanged to signal "not consumed by overlays".
    ControllerOutcome::Unchanged
}

/// Top-level mouse dispatcher.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, _area: Rect) -> ControllerOutcome {
    let col = mouse.column;
    let row = mouse.row;
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            scroll_active_list(app, true);
            ControllerOutcome::Changed
        }
        MouseEventKind::ScrollUp => {
            scroll_active_list(app, false);
            ControllerOutcome::Changed
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            // Click on sidebar tab → switch tab
            if let Some(sidebar) = app.sidebar_rect {
                if rect_contains(sidebar, col, row) {
                    let tabs = app.available_tabs();
                    let inner_y = row.saturating_sub(sidebar.y + 1);
                    if let Some(&tab) = tabs.get(inner_y as usize) {
                        if app.active_tab != tab {
                            app.active_tab = tab;
                            return ControllerOutcome::Changed;
                        }
                    }
                }
            }
            // Click on content row → select it
            if let Some(content) = app.content_rect {
                if rect_contains(content, col, row) {
                    let header_offset = 3u16;
                    if row >= content.y + header_offset {
                        let clicked_row = (row - content.y - header_offset) as usize;
                        if let Some(state) = app.active_table_state_mut() {
                            state.select(Some(clicked_row));
                            return ControllerOutcome::Changed;
                        }
                    }
                }
            }
            ControllerOutcome::Changed
        }
        _ => ControllerOutcome::Unchanged,
    }
}

fn scroll_active_list(app: &mut App, down: bool) {
    use crate::app::Tab;
    if let Some(s) = app.active_table_state_mut() {
        let selected = s.selected().unwrap_or(0);
        let new = if down {
            selected.saturating_add(1)
        } else {
            selected.saturating_sub(1)
        };
        s.select(Some(new));
    } else if app.active_tab == Tab::Terminal {
        if down {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(1);
        } else {
            app.terminal_scroll = app.terminal_scroll.saturating_add(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Returns `true` when (`col`, `row`) falls inside `rect` (inclusive).
#[inline]
pub fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Shrink `rect` by one cell on each side (the border of a `Block`).
/// Returns `Rect::ZERO` when the area is too small.
#[inline]
pub fn border_inner(rect: Rect) -> Rect {
    if rect.width < 2 || rect.height < 2 {
        Rect::ZERO
    } else {
        Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width - 2,
            height: rect.height - 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_basic() {
        let r = Rect::new(5, 5, 10, 10);
        assert!(rect_contains(r, 5, 5));
        assert!(rect_contains(r, 14, 14));
        assert!(!rect_contains(r, 15, 15));
        assert!(!rect_contains(r, 4, 5));
    }

    #[test]
    fn border_inner_shrinks() {
        let r = Rect::new(0, 0, 10, 10);
        let inner = border_inner(r);
        assert_eq!(inner, Rect::new(1, 1, 8, 8));
    }

    #[test]
    fn border_inner_too_small() {
        assert_eq!(border_inner(Rect::new(0, 0, 1, 1)), Rect::ZERO);
        assert_eq!(border_inner(Rect::new(0, 0, 0, 0)), Rect::ZERO);
    }
}
