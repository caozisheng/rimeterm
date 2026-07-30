//! Native dual-pane file manager backed by `tui-file-explorer`.

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_core::{EventBus, FileSide as KernelFileSide, KernelEvent};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tui_file_explorer::{
    App, AppOptions, AppOutcome, Editor, FileMutation, FileMutationFailure, FileMutationResult,
    HintLayout, Pane, copy_dir_all, draw_in,
};

/// Which explorer column currently receives keyboard input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSide {
    /// Left explorer column.
    Left,
    /// Right explorer column.
    Right,
}

impl From<Pane> for FileSide {
    fn from(value: Pane) -> Self {
        match value {
            Pane::Left => Self::Left,
            Pane::Right => Self::Right,
        }
    }
}

/// Native provider embedding the complete two-pane explorer application.
pub struct FileManagerPane {
    id: PaneId,
    title: String,
    app: App,
    pending_mutation: Option<FileMutation>,
    mutation_rx: Option<Receiver<FileMutationResult>>,
    event_bus: Option<EventBus>,
    last_snapshot: Option<Snapshot>,
    /// Rect of the embedded preview pane captured on the last render.
    /// Zero-sized when the upstream App is not showing a preview.
    preview_rect: Rect,
}

#[derive(Clone)]
struct Snapshot {
    side: KernelFileSide,
    cwd: PathBuf,
    highlighted: Option<PathBuf>,
}

impl FileManagerPane {
    /// Construct a dual-pane explorer rooted at the supplied directories.
    pub fn new(left_dir: PathBuf, right_dir: PathBuf) -> Self {
        let app = App::new(AppOptions {
            left_dir,
            right_dir,
            editor: Editor::None,
            // Native FileManagerPane lives in the top-left quadrant
            // (~35 % of a typical terminal). Horizontal hint pairs
            // truncate at that width; stack them vertically so every
            // key hint stays legible.
            hint_layout: HintLayout::Vertical,
            ..AppOptions::default()
        });
        Self {
            id: PaneId::next(),
            title: "Files".to_owned(),
            app,
            pending_mutation: None,
            mutation_rx: None,
            event_bus: None,
            last_snapshot: None,
            preview_rect: Rect::default(),
        }
    }
    /// Construct a file manager that broadcasts structured navigation events.
    pub fn with_event_bus(left_dir: PathBuf, right_dir: PathBuf, event_bus: EventBus) -> Self {
        let mut pane = Self::new(left_dir, right_dir);
        pane.event_bus = Some(event_bus);
        pane.last_snapshot = Some(pane.observed_snapshot());
        pane
    }

    /// Return the currently focused explorer column.
    pub fn active_side(&self) -> FileSide {
        self.app.active.into()
    }

    /// Return the focused column's current directory.
    pub fn active_dir(&self) -> &Path {
        &self.app.active_pane().current_dir
    }

    /// Returns `true` when the upstream App has an inline editor active
    /// (opened with `i`). Callers use this to suppress global shortcuts
    /// like the viewer overlay right-arrow so the editor gets raw input.
    pub fn is_editing(&self) -> bool {
        self.app.inline_editor.is_some()
    }

    /// Refresh the cached preview rect from the current `App` state.
    /// Mirrors the layout in upstream `draw_in`: the preview panel is
    /// the last horizontal chunk when `show_preview` is set.
    fn recompute_preview_rect(&mut self, area: Rect) {
        if !self.app.show_preview {
            self.preview_rect = Rect::default();
            return;
        }
        let action_bar_height = self.app.hint_layout.action_bar_rows();
        let main_height = area.height.saturating_sub(action_bar_height);
        if main_height == 0 || area.width == 0 {
            self.preview_rect = Rect::default();
            return;
        }
        // Preview takes the trailing 50% (single-pane) or 50% remainder
        // (dual pane 25%/25%/50%). Rimeterm never enables theme/options/
        // editor side panels, so the preview rect is the last chunk.
        let (preview_x, preview_width) = if self.app.single_pane {
            let list_w = area.width * 40 / 100;
            (area.x + list_w, area.width - list_w)
        } else {
            let list_w = area.width * 25 / 100;
            (area.x + list_w * 2, area.width - list_w * 2)
        };
        self.preview_rect = Rect::new(preview_x, area.y, preview_width, main_height);
    }

    /// Return the focused column's highlighted path, if any.
    pub fn highlighted_path(&self) -> Option<&Path> {
        self.app
            .active_pane()
            .current_entry()
            .map(|entry| entry.path.as_path())
    }

    /// Take a deferred filesystem mutation requested by the explorer.
    pub fn take_pending_mutation(&mut self) -> Option<FileMutation> {
        self.pending_mutation.take()
    }

    /// Reload both explorer columns from disk.
    pub fn reload(&mut self) {
        self.app.left.reload();
        self.app.right.reload();
    }
    fn observed_snapshot(&self) -> Snapshot {
        Snapshot {
            side: match self.active_side() {
                FileSide::Left => KernelFileSide::Left,
                FileSide::Right => KernelFileSide::Right,
            },
            cwd: self.active_dir().to_path_buf(),
            highlighted: self.highlighted_path().map(|p| p.to_path_buf()),
        }
    }

    fn emit_snapshot_diff(&mut self) {
        let Some(bus) = &self.event_bus else {
            return;
        };
        let next = self.observed_snapshot();
        let previous = self.last_snapshot.replace(next.clone());
        let cwd_changed = previous
            .as_ref()
            .map(|prev| prev.side != next.side || prev.cwd != next.cwd)
            .unwrap_or(true);
        let highlight_changed = previous
            .as_ref()
            .map(|prev| prev.highlighted != next.highlighted)
            .unwrap_or(next.highlighted.is_some());
        if cwd_changed {
            bus.send(KernelEvent::FileManagerCwdChanged {
                origin: self.id,
                side: next.side,
                path: next.cwd.clone(),
            });
        }
        if highlight_changed {
            if let Some(path) = next.highlighted {
                bus.send(KernelEvent::FileSelected {
                    origin: self.id,
                    side: next.side,
                    path,
                });
            }
        }
    }

    fn spawn_pending_mutation(&mut self) {
        let Some(mutation) = self.pending_mutation.take() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.mutation_rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(execute_mutation(mutation));
        });
    }
}
fn execute_mutation(mutation: FileMutation) -> FileMutationResult {
    let mut succeeded = Vec::new();
    let mut failures = Vec::new();
    let mut clear_clipboard = false;

    match mutation {
        FileMutation::Paste {
            sources,
            destination,
            is_cut,
            overwrite,
        } => {
            for source in sources {
                let Some(name) = source.file_name() else {
                    failures.push(FileMutationFailure {
                        path: source,
                        error: "source has no filename".to_owned(),
                    });
                    continue;
                };
                let target = destination.join(name);
                let result = (|| -> std::io::Result<()> {
                    if target.exists() && !overwrite {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "destination exists",
                        ));
                    }
                    if overwrite && target.exists() {
                        remove_path(&target)?;
                    }
                    if source.is_dir() {
                        copy_dir_all(&source, &target)?;
                    } else {
                        std::fs::copy(&source, &target)?;
                    }
                    if is_cut {
                        remove_path(&source)?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => succeeded.push(target),
                    Err(error) => failures.push(FileMutationFailure {
                        path: source,
                        error: error.to_string(),
                    }),
                }
            }
            clear_clipboard = is_cut && failures.is_empty();
        }
        FileMutation::Delete { paths } => {
            for path in paths {
                match remove_path(&path) {
                    Ok(()) => succeeded.push(path),
                    Err(error) => failures.push(FileMutationFailure {
                        path,
                        error: error.to_string(),
                    }),
                }
            }
        }
    }
    FileMutationResult {
        succeeded,
        failures,
        clear_clipboard,
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

impl PaneProvider for FileManagerPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        _ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        draw_in(&mut self.app, frame, area);
        self.recompute_preview_rect(area);
        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.app.handle_key_deferred(key) {
            Ok(AppOutcome::MutationRequested(mutation)) => {
                self.pending_mutation = Some(mutation);
                self.spawn_pending_mutation();
                true
            }
            Ok(_) => {
                self.emit_snapshot_diff();
                true
            }
            Err(error) => {
                self.app.status_msg = format!("Input failed: {error}");
                true
            }
        }
    }

    fn on_mouse(&mut self, event: MouseEvent, outer: Rect) -> bool {
        // Prefer the rect captured by the most recent render — it
        // reflects the exact upstream layout. Fall back to a fresh
        // computation against the host's outer rect when we haven't
        // rendered yet (unit tests, first frame after resize).
        if self.preview_rect.width == 0 || self.preview_rect.height == 0 {
            self.recompute_preview_rect(outer);
        }
        let point_in = |rect: Rect| {
            rect.width > 0
                && rect.height > 0
                && event.column >= rect.x
                && event.column < rect.right()
                && event.row >= rect.y
                && event.row < rect.bottom()
        };
        match event.kind {
            MouseEventKind::ScrollDown if point_in(self.preview_rect) => {
                self.app.preview_state.scroll_down(3);
                true
            }
            MouseEventKind::ScrollUp if point_in(self.preview_rect) => {
                self.app.preview_state.scroll_up(3);
                true
            }
            _ => false,
        }
    }

    fn poll_background(&mut self) -> bool {
        let Some(rx) = self.mutation_rx.as_ref() else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.mutation_rx = None;
        self.app.apply_mutation_result(result);
        true
    }

    fn reload(&mut self) {
        FileManagerPane::reload(self);
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use tempfile::tempdir;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn scroll_over_preview_area_scrolls_preview_state() {
        let dir = tempdir().unwrap();
        let mut pane = FileManagerPane::new(dir.path().into(), dir.path().into());
        pane.app.show_preview = true;
        pane.preview_rect = Rect::new(20, 0, 30, 20);
        let before = pane.app.preview_state.scroll;
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 25, 5),
            Rect::new(0, 0, 60, 20),
        );
        assert!(consumed);
        assert!(pane.app.preview_state.scroll > before);
    }

    #[test]
    fn scroll_up_over_preview_area_decreases_preview_scroll() {
        let dir = tempdir().unwrap();
        let mut pane = FileManagerPane::new(dir.path().into(), dir.path().into());
        pane.app.show_preview = true;
        pane.app.preview_state.scroll = 10;
        pane.preview_rect = Rect::new(20, 0, 30, 20);
        let _ = pane.on_mouse(
            mouse(MouseEventKind::ScrollUp, 25, 5),
            Rect::new(0, 0, 60, 20),
        );
        assert!(pane.app.preview_state.scroll < 10);
    }

    #[test]
    fn scroll_outside_preview_area_does_not_consume() {
        let dir = tempdir().unwrap();
        let mut pane = FileManagerPane::new(dir.path().into(), dir.path().into());
        pane.app.show_preview = true;
        pane.preview_rect = Rect::new(20, 0, 30, 20);
        let consumed = pane.on_mouse(
            mouse(MouseEventKind::ScrollDown, 5, 5),
            Rect::new(0, 0, 60, 20),
        );
        assert!(!consumed);
    }

    #[test]
    fn is_editing_matches_inline_editor_state() {
        let dir = tempdir().unwrap();
        let pane = FileManagerPane::new(dir.path().into(), dir.path().into());
        assert!(!pane.is_editing());
    }
}
