use ratatui::{Frame, layout::Rect};
use rimeterm_core::pane::{PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};

struct NativeFramePane(PaneId);

impl PaneProvider for NativeFramePane {
    fn id(&self) -> PaneId {
        self.0
    }

    fn title(&self) -> &str {
        "native"
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        _ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let _ = frame.buffer_mut().area().intersection(area);
        RenderOutcome::default()
    }
}

#[test]
fn pane_provider_accepts_frame_native_renderers() {
    let pane: Box<dyn PaneProvider> = Box::new(NativeFramePane(PaneId::next()));
    assert_eq!(pane.title(), "native");
}
