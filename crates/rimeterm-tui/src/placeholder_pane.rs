//! Placeholder native pane for cells whose real plugin (yazi, gitui, bottom,
//! omp, …) hasn't landed yet. Shows a bordered box with the group / adapter
//! name and a subtle "pending" hint. Fully swappable — the app spawns it into
//! the [`PaneRegistry`] under the correct kind and swaps to a real provider
//! once the corresponding milestone lands.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

/// A do-nothing pane that draws its own name.
pub struct PlaceholderPane {
    id: PaneId,
    title: String,
    subtitle: String,
    icon: String,
    color: Color,
    /// Populated when this placeholder stands in for a registered external
    /// tool: pressing `[I]` while focused opens a new shell tab with this
    /// command pre-typed (see `PaneProvider::install_command`).
    install_command: Option<String>,
}

impl PlaceholderPane {
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        icon: impl Into<String>,
        color: Color,
    ) -> Self {
        Self {
            id: PaneId::next(),
            title: title.into(),
            subtitle: subtitle.into(),
            icon: icon.into(),
            color,
            install_command: None,
        }
    }

    /// Attach a one-key install command. Returns `self` for builder-style
    /// chaining at the callsite (see `build_external_pane`). Passing an
    /// empty / all-whitespace string strips it back to `None` so bad
    /// callers don't leak whitespace-only shortcuts.
    pub fn with_install_command(mut self, cmd: impl Into<String>) -> Self {
        let cmd = cmd.into();
        self.install_command = if cmd.trim().is_empty() {
            None
        } else {
            Some(cmd)
        };
        self
    }
}

impl PaneProvider for PlaceholderPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn set_title(&mut self, title: String) -> bool {
        self.title = title;
        true
    }

    fn install_command(&self) -> Option<&str> {
        self.install_command.as_deref()
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &PaneRenderCtx) -> RenderOutcome {
        // Focus visuals: match PtyPane — focused = bright + bold + `▶` marker,
        // unfocused = dim grey. Placeholder-specific `self.color` still tints
        // the border on focus so different tools stay visually distinct.
        let marker = if ctx.focused { "▶ " } else { "  " };
        let heading = format!(" {}{} {} ", marker, self.icon, self.title);
        let border_style = if ctx.focused {
            Style::default().fg(self.color).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let block = Block::default()
            .title(heading)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);

        // The subtitle may be a single sentence (e.g. picker hint) or a
        // multi-line install block from `InstallHint::to_string()`. Render
        // the first line as a bold centered heading; if further lines
        // exist, render them left-aligned + dimmed as a body block. Left
        // alignment matters because "  Windows: cmd" only reads as a table
        // when the leading whitespace lands at a fixed column.
        let mut lines = self.subtitle.split('\n');
        let first = lines.next().unwrap_or("");
        let rest: Vec<&str> = lines.collect();

        let mut body: Vec<Line<'_>> = Vec::with_capacity(2 + rest.len());
        body.push(Line::from(Span::styled(
            first,
            Style::default().fg(self.color).add_modifier(Modifier::BOLD),
        )));
        if !rest.is_empty() {
            body.push(Line::from(""));
            for row in &rest {
                body.push(Line::from(Span::styled(
                    *row,
                    Style::default().add_modifier(Modifier::DIM),
                )));
            }
        }
        // Discoverability: when an install shortcut is wired up, tell the
        // user which key runs it. Uses the same `[I]` prefix the placeholder
        // itself intercepts, so keybind and label stay in lockstep.
        if self.install_command.is_some() {
            body.push(Line::from(""));
            body.push(Line::from(Span::styled(
                "[I] Install now",
                Style::default().fg(self.color).add_modifier(Modifier::BOLD),
            )));
        }
        // Center the whole block when it's a single-line hint (aesthetic
        // fit for the picker "🤖 Pick an agent"); left-align when it's a
        // multi-line install prompt (columns must line up).
        let alignment = if rest.is_empty() {
            Alignment::Center
        } else {
            Alignment::Left
        };
        Paragraph::new(body).alignment(alignment).render(inner, buf);
        RenderOutcome::default()
    }

    fn on_key(&mut self, _key: KeyEvent) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_defaults_to_none() {
        let p = PlaceholderPane::new("bottom", "not installed", "📊", Color::DarkGray);
        assert!(p.install_command().is_none());
    }

    #[test]
    fn with_install_command_sets_command() {
        let p = PlaceholderPane::new("bottom", "not installed", "📊", Color::DarkGray)
            .with_install_command("cargo install --locked bottom");
        assert_eq!(p.install_command(), Some("cargo install --locked bottom"));
    }

    #[test]
    fn with_install_command_empty_strips_to_none() {
        // Callers who accidentally pass "" or "   " shouldn't create a
        // [I] shortcut that would type nothing into a new shell.
        let p = PlaceholderPane::new("x", "y", "z", Color::DarkGray).with_install_command("   ");
        assert!(p.install_command().is_none());

        let p = PlaceholderPane::new("x", "y", "z", Color::DarkGray).with_install_command("");
        assert!(p.install_command().is_none());
    }

    #[test]
    fn set_title_replaces_title() {
        let mut p = PlaceholderPane::new("old", "sub", "i", Color::DarkGray);
        assert_eq!(p.title(), "old");
        assert!(p.set_title("new".to_string()));
        assert_eq!(p.title(), "new");
    }
}
