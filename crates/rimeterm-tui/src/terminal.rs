//! Terminal setup, in-memory frame composition, and off-thread presentation.
//!
//! The UI renders each frame into a [`TestBackend`] only; no stdout call is made on the input /
//! render thread. The completed buffer and cursor are handed to the presentation worker, which
//! coalesces snapshots (latest wins) and writes synchronized, full-frame diffs from its own
//! thread. A slow outer terminal therefore cannot stall input handling.

use std::io;

use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

use crate::presentation::Presenter;

pub type TuiTerminal = Terminal<TestBackend>;

/// Guard that restores the terminal on drop.
///
/// Dropping waits for the presenter to finish any in-flight write and restore the terminal,
/// because raw mode must not be disabled while the worker can still emit bytes. Normal draws
/// never wait for stdout.
pub struct TerminalGuard {
    terminal: TuiTerminal,
    presenter: Presenter,
    area: Rect,
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        let (width, height) = crossterm::terminal::size()?;
        enable_raw_mode()?;

        // TestBackend is ratatui's in-memory backend: `Terminal::draw` performs all diffing and
        // cursor bookkeeping here, so the UI never touches the Crossterm backend.
        let terminal = match Terminal::new(TestBackend::new(width, height)) {
            Ok(terminal) => terminal,
            Err(never) => match never {},
        };

        let presenter = match Presenter::start() {
            Ok(presenter) => presenter,
            Err(error) => {
                let _ = disable_raw_mode();
                return Err(error.into());
            }
        };

        Ok(Self {
            terminal,
            presenter,
            area: Rect::new(0, 0, width, height),
        })
    }

    /// Compose one complete frame in memory and submit it to the presenter.
    ///
    /// Returns an output error from the presenter (reported once) so failures are not silent.
    /// The closure still receives the same `Frame` handle as before and returns the caret
    /// position to show, or `None` to keep it hidden.
    pub fn draw(
        &mut self,
        render: impl FnOnce(&mut Frame<'_>) -> Option<(u16, u16)>,
    ) -> io::Result<()> {
        if let Some(error) = self.presenter.take_error() {
            return Err(error);
        }

        // A terminal-size query only (ioctl), never stdout output. Resize the in-memory backend
        // and ratatui's buffers so the next render uses the current geometry.
        let (width, height) = crossterm::terminal::size()?;
        let area = Rect::new(0, 0, width, height);
        if area != self.area {
            self.terminal.backend_mut().resize(width, height);
            if let Err(never) = self.terminal.resize(area) {
                match never {}
            }
            self.area = area;
        }

        let mut cursor = None;
        let completed = match self.terminal.draw(|frame| {
            cursor = render(frame);
        }) {
            Ok(completed) => completed,
            Err(never) => match never {},
        };

        self.presenter.submit(completed.buffer, cursor);
        if let Some(error) = self.presenter.take_error() {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Join the worker first: it restores cursor shape / alt screen / mouse capture and
        // flushes them. Only after it has ended is it safe to disable raw mode, and no bytes can
        // reach the terminal afterwards.
        self.presenter.shutdown();
        if let Err(e) = disable_raw_mode() {
            tracing::warn!(error = %e, "disable_raw_mode failed on shutdown");
        }
    }
}
