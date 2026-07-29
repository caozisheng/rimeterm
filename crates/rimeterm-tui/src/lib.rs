//! ratatui front-end: main loop, status bar, PTY pane, app-menu popover,
//! command palette, tab strip, keymap engine.
//!
//! M1 introduces multi-shell tabs, focus management, and the command palette.

pub mod acknowledgement;
pub mod agent_factory;
pub mod agtop_matchers;
pub mod agtop_model;
pub mod agtop_pane;
pub mod agtop_worker;
pub mod app;
pub mod diff_highlight;
pub mod file_manager_pane;
pub mod git_model;
pub mod git_pane;
pub mod git_worker;
pub mod keymap;
pub mod menu;
pub mod palette;
pub mod pane_registry;
pub mod picker;
pub mod placeholder_pane;
pub mod pty_pane;
pub mod pty_selection;
pub mod settings;
pub mod shell_factory;
pub(crate) mod shell_integration;
pub mod status_bar;
pub mod sysmon_model;
pub mod sysmon_pane;
pub mod sysmon_worker;
pub mod tab_strip;
pub mod terminal;
#[cfg(windows)]
pub mod updater;
#[cfg(windows)]
pub mod upgrade;
pub mod viewer;

pub use app::App;
