//! Kernel primitives for rimeterm.
//!
//! Design map: this crate hosts the load-bearing traits and types the rest of
//! the workspace speaks. Everything else — TUI front-end, PTY host, WASM host,
//! agent adapters — depends on these shapes. Nothing here does I/O.
//!
//! - [`pane`]: [`PaneProvider`] trait and [`PaneId`] — the single visual unit.
//! - [`event`]: [`KernelEvent`] and [`EventBus`] — the only inter-plugin channel.
//! - [`command`]: [`Command`] + [`CommandRegistry`] — palette / keymap / rimectl entrypoint.
//! - [`app_menu`]: the top-left `≡ rimeterm` popover; §19.13 of the design doc.
//!
//! See `docs/rimeterm-overall-design.md` for the design contract these types serve.

pub mod app_menu;
pub mod command;
pub mod event;
pub mod focus;
pub mod layout;
pub mod pane;
pub mod tabs;

pub use app_menu::{AppMenu, AppMenuItem};
pub use command::{Command, CommandId, CommandRegistry};
pub use event::{EventBus, KernelEvent};
pub use focus::FocusManager;
pub use layout::{LayoutNode, LayoutTree};
pub use pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
pub use tabs::{
    MembersPolicy, PaneKind, PolicyError, TabGroup, TabGroupId, BUILTIN_AGENTS, BUILTIN_FILES,
    BUILTIN_SHELLS, BUILTIN_SYSMON,
};
