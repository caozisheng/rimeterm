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
        }
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

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &PaneRenderCtx) -> RenderOutcome {
        let heading = format!(" {} {} ", self.icon, self.title);
        let mut block = Block::default().title(heading).borders(Borders::ALL);
        if ctx.focused {
            block = block.border_style(Style::default().fg(self.color));
        } else {
            block = block.border_style(Style::default().add_modifier(Modifier::DIM));
        }
        let inner = block.inner(area);
        block.render(area, buf);

        let body = vec![
            Line::from(Span::styled(
                self.subtitle.as_str(),
                Style::default()
                    .fg(self.color)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "coming in a later milestone",
                Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC),
            )),
        ];
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .render(inner, buf);
        RenderOutcome::default()
    }

    fn on_key(&mut self, _key: KeyEvent) -> bool {
        false
    }
}
