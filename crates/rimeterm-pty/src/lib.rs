//! PTY host for rimeterm.
//!
//! Wraps [`portable_pty`] (ConPTY on Windows 1809+, openpty on Unix) and feeds
//! bytes into a [`vt100::Parser`] whose grid drives the PtyPane provider.
//!
//! v0.1 exposes one [`Session`] with a single spawned child. Multi-shell / tab
//! groups sit on top in later crates.

pub mod session;
pub mod agent_detect;
pub mod agent_registry;
pub mod shell_detect;
pub mod resize_throttle;

pub use session::{PtyBackend, Session, SessionConfig, SessionError, SessionOutput};
pub use shell_detect::{detect_default_shell, ShellChoice};
pub use agent_detect::{detect_agent, detect_tool, AgentAvailability, ToolAvailability};
pub use resize_throttle::{Decision, ResizeThrottle, PLATFORM_RESIZE_DEBOUNCE};
