use crossterm::event::{KeyEvent, MouseEvent};
use glab_tui::controller::HostAction;
use glab_tui::embed::{
    AppShell, EmbeddedApp, EmbeddedOptions, EmbeddedOutcome, EmbeddedState, HostActionResult,
};
use ratatui::{Frame, layout::Rect, style::Color};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use std::any::Any;
use std::path::{Path, PathBuf};

pub struct GlabPane {
    id: PaneId,
    root: PathBuf,
    app: EmbeddedApp,
    /// Preserved for theme-colour compatibility (Task 11). The full
    /// embedded UI receives its palette through `AppResources`; this
    /// field keeps the `GlabPane::new` signature stable so the host
    /// callsite can still forward its resolved accent colour.
    _theme: Color,
    /// Latched when the embedded controller returns
    /// [`EmbeddedOutcome::ExitRequested`]; the host clears it after
    /// acting (e.g. switching focus away from this pane).
    exit_requested: bool,
}

impl GlabPane {
    pub fn new(root: PathBuf, theme: Color, handle: tokio::runtime::Handle) -> Self {
        let options = EmbeddedOptions {
            workspace_root: root.clone(),
            initial_tab: None,
            cache_policy: Default::default(),
            refresh: Some(std::time::Duration::from_secs(300)),
            features: Default::default(),
        };
        Self {
            id: PaneId::next(),
            app: EmbeddedApp::new(options, handle),
            root,
            _theme: theme,
            exit_requested: false,
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn refresh_for(&mut self, root: &Path) {
        self.root = root.to_path_buf();
        self.app.set_workspace_root(root);
    }

    /// Whether the embedded controller signalled an exit intent.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Clear the latched exit flag after the host has acted on it.
    pub fn clear_exit_request(&mut self) {
        self.exit_requested = false;
    }

    // -- Persistence (Task 10) ---------------------------------------------

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let state = self.app.snapshot();
        let mut values = std::collections::BTreeMap::new();
        if let Ok(tab_json) = serde_json::to_string(&state.active_tab) {
            values.insert("active_tab".into(), tab_json);
        }
        values.insert("generation".into(), state.generation.to_string());
        rimeterm_config::memory_state::PaneState { values }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        let Some(tab_str) = state.values.get("active_tab") else {
            return;
        };
        let Ok(tab) = serde_json::from_str(tab_str) else {
            return;
        };
        let generation = state
            .values
            .get("generation")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let embedded_state = EmbeddedState {
            workspace_root: self.root.clone(),
            active_tab: tab,
            generation,
        };
        self.app.restore(embedded_state);
    }

    // -- Outcome handling --------------------------------------------------

    fn handle_outcome(&mut self, outcome: EmbeddedOutcome) -> bool {
        match outcome {
            EmbeddedOutcome::Unchanged => false,
            EmbeddedOutcome::Changed => true,
            EmbeddedOutcome::ExitRequested => {
                self.exit_requested = true;
                true
            }
            EmbeddedOutcome::HostAction(action) => {
                self.handle_host_action(action);
                true
            }
        }
    }

    fn handle_host_action(&mut self, action: HostAction) {
        match action {
            HostAction::CopyText(text) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
                self.app
                    .complete_host_action(HostActionResult::CopyCompleted);
            }
            HostAction::OpenUrl(_) | HostAction::EditText { .. } => {
                // These require terminal ownership — the host main loop
                // should intercept them before they reach this fallback.
                self.app.complete_host_action(HostActionResult::Cancelled);
            }
        }
    }
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
        _ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        self.app.render(frame, area);
        RenderOutcome {
            request_redraw: matches!(self.app.shell(), AppShell::Detecting),
            cursor: None,
        }
    }
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let outcome = self.app.handle_key(key);
        self.handle_outcome(outcome)
    }
    fn on_mouse(&mut self, event: MouseEvent, area: Rect) -> bool {
        let outcome = self.app.handle_mouse(event, area);
        self.handle_outcome(outcome)
    }
    fn poll_background(&mut self) -> bool {
        self.app.poll_background()
    }
    fn reload(&mut self) {
        self.app.reload();
    }
    fn set_visible(&mut self, visible: bool) {
        self.app.set_visible(visible);
    }
    fn scrollbar_dragging(&self) -> bool {
        // The full embedded UI manages its own scroll state internally.
        false
    }
    fn has_active_selection(&self) -> bool {
        // Selection is managed inside the full UI's controller layer.
        false
    }
    fn wants_mouse_priority(&self, _shift_held: bool) -> bool {
        // The full embedded UI always needs mouse events for its
        // tab-strip, list interactions, and internal scrollbar.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn make_handle() -> tokio::runtime::Handle {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let h = rt.handle().clone();
        std::mem::forget(rt); // keep alive for the test
        h
    }

    #[test]
    fn pane_title_and_root() {
        let pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White, make_handle());
        assert_eq!(pane.title(), "Glab");
        assert_eq!(pane.workspace_root(), Path::new("C:/repo"));
    }

    #[test]
    fn pane_forwards_keys() {
        let mut pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White, make_handle());
        // In Detecting state most keys are no-ops (Unchanged → false).
        let consumed = pane.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        // We don't assert the value — the point is it doesn't panic.
        let _ = consumed;
    }

    #[test]
    fn exit_requested_latch() {
        let pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White, make_handle());
        assert!(!pane.exit_requested());
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let pane = GlabPane::new(PathBuf::from("C:/repo"), Color::White, make_handle());
        let state = pane.snapshot_state();
        // Must contain active_tab and generation keys.
        assert!(state.values.contains_key("active_tab"));
        assert!(state.values.contains_key("generation"));

        let mut pane2 = GlabPane::new(PathBuf::from("C:/repo"), Color::White, make_handle());
        pane2.restore_state(&state); // should not panic
    }
}
