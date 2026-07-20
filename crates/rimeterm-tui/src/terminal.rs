//! Raw-mode terminal setup + teardown.
//!
//! Wraps crossterm's alt-screen + raw-mode dance so the caller can `?` its way
//! through the setup and always unwind on drop.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Guard that restores the terminal on drop. Safe to leak on panic — the
/// [`std::io::stdout`] handle stays valid and the guard just resets modes.
pub struct TerminalGuard {
    /// Kept for API symmetry: exposes the ratatui Terminal to callers.
    pub terminal: TuiTerminal,
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore; log if something goes sideways but never panic
        // during unwind.
        if let Err(e) = disable_raw_mode() {
            tracing::warn!(error = %e, "disable_raw_mode failed on shutdown");
        }
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture) {
            tracing::warn!(error = %e, "LeaveAlternateScreen failed on shutdown");
        }
    }
}
