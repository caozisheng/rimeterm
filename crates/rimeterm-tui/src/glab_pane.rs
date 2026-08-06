use crossterm::event::{KeyEvent, MouseEvent};
use glab_tui::EmbeddedApp;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use std::any::Any;
use std::path::{Path, PathBuf};

pub struct GlabPane {
    id: PaneId,
    root: PathBuf,
    app: EmbeddedApp,
    theme: Color,
}
impl GlabPane {
    pub fn new(root: PathBuf, theme: Color) -> Self {
        Self {
            id: PaneId::next(),
            app: EmbeddedApp::new(&root, theme),
            root,
            theme,
        }
    }
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }
    pub fn refresh_for(&mut self, root: &Path) {
        self.root = root.to_path_buf();
        self.app.set_workspace_root(root);
    }
    pub fn snapshot(&self) -> &glab_tui::GlabSnapshot {
        self.app.snapshot()
    }
    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        rimeterm_config::memory_state::PaneState {
            values: std::collections::BTreeMap::new(),
        }
    }
    pub(crate) fn restore_state(&mut self, _state: &rimeterm_config::memory_state::PaneState) {}
}
impl PaneProvider for GlabPane {
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
    fn id(&self) -> PaneId {
        self.id
    }
    fn title(&self) -> &str {
        "Glab"
    }
    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }
    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        self.app
            .render_with_context(frame, area, ctx.focused, ctx.focus_color);
        RenderOutcome {
            request_redraw: matches!(self.app.snapshot().status, glab_tui::GlabStatus::Loading),
            cursor: None,
        }
    }
    fn on_key(&mut self, key: KeyEvent) -> bool {
        self.app.handle_key(key)
    }
    fn on_mouse(&mut self, event: MouseEvent, area: Rect) -> bool {
        self.app.on_mouse(event, area)
    }
    fn poll_background(&mut self) -> bool {
        self.app.poll_background()
    }
    fn reload(&mut self) {
        self.app.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    #[test]
    fn pane_uses_native_embedded_app() {
        let pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White);
        assert_eq!(pane.title(), "Glab");
        assert_eq!(pane.workspace_root(), Path::new("C:/repo"));
    }

    #[test]
    fn pane_forwards_keys_without_pty() {
        let mut pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White);
        assert!(pane.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
    }

    #[test]
    fn focused_render_uses_context_focus_color() {
        let mut pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White);
        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                pane.render(
                    Rect::new(0, 0, 30, 8),
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: Color::Magenta,
                    },
                );
            })
            .unwrap();
        assert_eq!(
            terminal.backend().buffer().cell((0, 0)).unwrap().fg,
            Color::Magenta
        );
    }
}
