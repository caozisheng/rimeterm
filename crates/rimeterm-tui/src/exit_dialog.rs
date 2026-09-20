//! Close confirmation dialog (daemon mode).
//!
//! When `sessiond` hosting is on and the user quits rimeterm — `[×]`
//! click, `Ctrl+Q`, or the app-menu command — this modal asks what to
//! do with the still-live sessions under the daemon:
//!
//! - **Keep running (detach)**: default. The TUI exits, the daemon keeps
//!   hosting every child, and the next launch reattaches. The daemon
//!   still exits by itself once its grace period elapses with no
//!   attached client (see `sessiond.state.toml`).
//! - **Kill now**: the TUI asks the daemon to kill every session and
//!   shut down before exiting (`ClientMsg::Shutdown`).
//!
//! Same overlay ergonomics as other modals: `Esc` cancels, `j/k` or
//! arrows move, `Enter` confirms. `Tab`/`l/h` toggle between the two
//! choices as well, mirroring the Settings tab switching feel.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

/// Which choice the cursor is on.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ExitChoice {
    #[default]
    KeepRunning,
    KillNow,
}

/// What the user picked once the dialog resolves.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExitDecision {
    /// Detach: exit the TUI, leave the daemon + children running.
    KeepRunning,
    /// Kill every session and shut the daemon down, then exit.
    KillNow,
}

/// State of the close-confirmation dialog.
#[derive(Default)]
pub struct ExitDialogState {
    pub open: bool,
    pub choice: ExitChoice,
}

impl ExitDialogState {
    pub fn open(&mut self) {
        self.choice = ExitChoice::KeepRunning;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Handle one key. `Some(decision)` resolves the dialog (and closes
    /// it); `None` means the dialog is still up (or was cancelled, in
    /// which case `open` flipped to `false` with no decision).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ExitDecision> {
        if !self.open {
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.open = false;
                None
            }
            KeyCode::Enter => {
                self.open = false;
                Some(match self.choice {
                    ExitChoice::KeepRunning => ExitDecision::KeepRunning,
                    ExitChoice::KillNow => ExitDecision::KillNow,
                })
            }
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Char('h') => {
                self.choice = match self.choice {
                    ExitChoice::KeepRunning => ExitChoice::KillNow,
                    ExitChoice::KillNow => ExitChoice::KeepRunning,
                };
                None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.choice = match self.choice {
                    ExitChoice::KeepRunning => ExitChoice::KillNow,
                    ExitChoice::KillNow => ExitChoice::KeepRunning,
                };
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.choice = match self.choice {
                    ExitChoice::KeepRunning => ExitChoice::KillNow,
                    ExitChoice::KillNow => ExitChoice::KeepRunning,
                };
                None
            }
            // `q` mirrors the acknowledgement overlay's quick-close.
            KeyCode::Char('q') if key.modifiers.is_empty() => {
                self.open = false;
                None
            }
            _ => None,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if !self.open {
            return;
        }
        let width = 56u16.min(area.width.saturating_sub(2));
        let height = 7u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup = Rect {
            x,
            y,
            width,
            height,
        };
        Clear.render(popup, buf);
        let block = Block::default()
            .title(" Close rimeterm? ")
            .borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);

        let sel = Style::default().add_modifier(Modifier::REVERSED);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let rows = [
            (
                "  → Keep sessions running (detach)",
                "    daemon exits after the grace period",
                self.choice == ExitChoice::KeepRunning,
            ),
            (
                "    Kill sessions & exit now",
                "    every child is terminated immediately",
                self.choice == ExitChoice::KillNow,
            ),
        ];
        let mut lines: Vec<Line> = Vec::new();
        for (label, hint, selected) in rows {
            let style = if selected { sel } else { Style::default() };
            lines.push(Line::styled(label, style));
            lines.push(Line::styled(hint, dim));
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

/// Whether a key press with modifiers should even reach the dialog.
/// `Ctrl+Q` arrives with CONTROL set; plain chars without it.
pub fn plain(key: &KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}
