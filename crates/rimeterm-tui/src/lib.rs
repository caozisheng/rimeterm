//! ratatui front-end: main loop, status bar, PTY pane, app-menu popover,
//! command palette, tab strip, keymap engine.
//!
//! M1 introduces multi-shell tabs, focus management, and the command palette.

pub mod acknowledgement;
pub mod agent_factory;
pub mod agtop_matchers;
pub mod agtop_model;
pub mod agtop_omp;
pub mod agtop_pane;
pub mod agtop_pricing;
pub mod agtop_session;
pub mod agtop_worker;
pub mod app;
pub mod diff_highlight;
pub mod file_manager_pane;
pub mod fr_pane;
pub mod git_model;
pub mod git_pane;
pub mod git_worker;
pub mod glab_pane;
pub mod keymap;
pub mod menu;
pub mod models_model;
pub mod models_pane;
pub mod models_worker;
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
pub mod stock_model;
pub mod stock_pane;
pub mod stock_worker;
pub mod sysmon_model;
pub mod sysmon_pane;
pub mod sysmon_worker;
pub mod tab_strip;
pub mod terminal;
pub mod todo_pane;
pub mod updater;
pub mod upgrade;
pub mod viewer;
pub mod zones_pane;

pub use app::App;
