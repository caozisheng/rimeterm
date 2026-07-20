//! ratatui front-end: main loop, status bar, PTY pane, app-menu popover,
//! command palette, tab strip, keymap engine.
//!
//! M1 introduces multi-shell tabs, focus management, and the command palette.

pub mod agent_factory;
pub mod app;
pub mod keymap;
pub mod menu;
pub mod palette;
pub mod pane_registry;
pub mod picker;
pub mod placeholder_pane;
pub mod pty_pane;
pub mod shell_factory;
pub mod status_bar;
pub mod tab_strip;
pub mod terminal;

pub use app::App;
