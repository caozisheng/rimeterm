//! C20 Viewer Overlay — snapshot classification and geometry helpers.
//!
//! The overlay is a modal Snapshot: `Alt+V` freezes the last `files:yazi:active`
//! selection and opens a read-only viewer. Yazi keeps its native third-column
//! Quick Look; rimeterm never proxies Yazi's preview widget.
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget,
    Widget, Wrap,
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::pty_selection::{Cell as SelCell, SelectionState};

/// Configured byte cap for Markdown snapshots (§19.11.2).
pub const MARKDOWN_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Configured byte cap for image snapshots (§19.11.2).
pub const IMAGE_MAX_BYTES: u64 = 40 * 1024 * 1024;

/// Minimum usable overlay width in cells. Falls back to Yazi Quick Look
/// when the terminal cannot host it (§19.5, §19.11).
pub const OVERLAY_MIN_COLS: u16 = 48;

/// Minimum usable overlay height in cells (§19.11).
pub const OVERLAY_MIN_ROWS: u16 = 16;

/// Percentage of the workspace the overlay occupies when opened (§19.11).
pub const OVERLAY_PERCENT_W: u16 = 90;
pub const OVERLAY_PERCENT_H: u16 = 90;

/// Minimum outer margin (cells) around the overlay so users can still see
/// the workspace behind it. Kept in sync with §19.2.
pub const OVERLAY_MARGIN: u16 = 2;

/// The kind of snapshot the modal viewer will render.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ViewerKind {
    Markdown,
    Image,
}

/// A frozen viewer source. Constructed via [`classify_source`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewerSource {
    pub path: PathBuf,
    pub kind: ViewerKind,
}

/// The active-Yazi selection kernel state. `Alt+V` copies this into a
/// snapshot iff `origin` is still the active files-group pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionSnapshot {
    pub origin: PaneId,
    pub path: PathBuf,
}

/// Reason for refusing to open the viewer. Surfaced as a status-bar hint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClassifyError {
    /// Extension is unknown / unsupported. Yazi Quick Look continues to
    /// preview it in-place (§19.11.5).
    Unsupported,
    /// Path exists but isn't a regular file (dir, symlink loop, device).
    NotRegularFile,
    /// File is larger than the per-kind byte cap.
    TooLarge { size: u64, cap: u64 },
    /// Path missing or unreadable; the caller stashes the OS error string.
    Unreadable(String),
}

/// Metadata needed to classify a path. Split out so tests can inject
/// fake sizes without touching the filesystem.
#[derive(Copy, Clone, Debug)]
pub struct SourceMeta {
    pub is_regular_file: bool,
    pub len: u64,
}

/// Pure classifier: extension → kind, size caps, regular-file check.
///
/// Returns `Ok(None)` when the extension isn't supported — the caller
/// treats that as "leave the file to Yazi Quick Look" without erroring.
/// Returns `Ok(Some(source))` when the snapshot is admissible.
pub fn classify_source(
    path: &Path,
    meta: SourceMeta,
) -> Result<Option<ViewerSource>, ClassifyError> {
    let Some(kind) = kind_for_extension(path) else {
        return Ok(None);
    };
    if !meta.is_regular_file {
        return Err(ClassifyError::NotRegularFile);
    }
    let cap = match kind {
        ViewerKind::Markdown => MARKDOWN_MAX_BYTES,
        ViewerKind::Image => IMAGE_MAX_BYTES,
    };
    if meta.len > cap {
        return Err(ClassifyError::TooLarge {
            size: meta.len,
            cap,
        });
    }
    Ok(Some(ViewerSource {
        path: path.to_path_buf(),
        kind,
    }))
}

/// The supported markdown extensions (§19.11.2).
const MARKDOWN_EXTS: &[&str] = &["md", "markdown"];
/// The supported image extensions (§19.11.2). `svg` is deliberately
/// excluded — `ratatui-image` does not render vector graphics.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

fn kind_for_extension(path: &Path) -> Option<ViewerKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if MARKDOWN_EXTS.contains(&ext.as_str()) {
        Some(ViewerKind::Markdown)
    } else if IMAGE_EXTS.contains(&ext.as_str()) {
        Some(ViewerKind::Image)
    } else {
        None
    }
}

/// Compute the centered overlay rectangle inside `bounds`.
///
/// Returns `None` when the terminal cannot host the overlay (below the
/// documented [`OVERLAY_MIN_COLS`] × [`OVERLAY_MIN_ROWS`] floor). Callers
/// surface a "terminal too narrow, viewer folded" hint (§19.11.5) instead
/// of opening a squashed modal.
pub fn overlay_rect(bounds: Rect) -> Option<Rect> {
    if bounds.width < OVERLAY_MIN_COLS || bounds.height < OVERLAY_MIN_ROWS {
        return None;
    }
    let usable_w = bounds.width.saturating_sub(OVERLAY_MARGIN * 2);
    let usable_h = bounds.height.saturating_sub(OVERLAY_MARGIN * 2);
    let target_w = percent(bounds.width, OVERLAY_PERCENT_W).max(OVERLAY_MIN_COLS);
    let target_h = percent(bounds.height, OVERLAY_PERCENT_H).max(OVERLAY_MIN_ROWS);
    let w = target_w.min(usable_w.max(OVERLAY_MIN_COLS));
    let h = target_h.min(usable_h.max(OVERLAY_MIN_ROWS));
    let x = bounds.x + (bounds.width - w) / 2;
    let y = bounds.y + (bounds.height - h) / 2;
    Some(Rect {
        x,
        y,
        width: w,
        height: h,
    })
}

fn percent(total: u16, pct: u16) -> u16 {
    ((u32::from(total) * u32::from(pct)) / 100) as u16
}

/// Where focus should return to after the modal closes. `None` means
/// "keep whatever pane is currently focused" (e.g. viewer opened by
/// palette without a live focus).
pub type ReturnFocus = Option<PaneId>;

/// Monotonic non-zero counter identifying an open snapshot. Every
/// `open_snapshot` bumps it so late worker completions (§19.11.3) can
/// discard their results without clobbering a newer snapshot.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Generation(pub u64);

/// Payload carried by worker completions. `pane_id`, `generation`, and
/// `path` must all match a live viewer tab's state for the completion
/// to apply — stale results (tab closed, snapshot bumped, wrong file)
/// are silently discarded.
#[derive(Debug)]
pub struct ViewerCompletion {
    pub pane_id: PaneId,
    pub generation: Generation,
    pub path: PathBuf,
    pub payload: ViewerPayload,
}

/// Loaded content produced by the workers. Not `Clone` — Markdown text
/// and image protocol state are large, so we move them into the state.
#[derive(Debug)]
pub enum ViewerPayload {
    Markdown(String),
    /// Placeholder for the image protocol state built in Task 6.
    Image(ImageReady),
    /// Terminal I/O or decode failure surfaced from the worker.
    Error(String),
}
/// Decoded image ready to be handed to `ratatui-image::Picker::new_protocol`
/// at render time. The heavy `DynamicImage` allocation lives here, not in
/// [`ViewerOverlayState`], so `Clone` on the state stays cheap.
pub struct ImageReady {
    pub image: image::DynamicImage,
}

impl std::fmt::Debug for ImageReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageReady")
            .field("width", &self.image.width())
            .field("height", &self.image.height())
            .finish()
    }
}

/// Modal lifecycle status. Every transition is driven by exactly one
/// `ViewerOverlayState` method — the caller never mutates fields
/// directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewerStatus {
    /// Overlay is not on screen. Snapshot / return-focus are cleared.
    Closed,
    /// `Alt+V` accepted the snapshot; a worker is preparing content.
    Loading,
    /// Content ready. Scroll / zoom keys apply to this state only.
    Ready,
    /// Worker returned an error, but the snapshot stays open so the
    /// user can still `Ctrl+O` to hand the file to the system app.
    Error(String),
}

/// Modal viewer state (§19.11.2). Owned by `App`; `PaneProvider::render`
/// only reads the prepared payload — no I/O in the render path.
#[derive(Debug)]
pub struct ViewerOverlayState {
    status: ViewerStatus,
    snapshot: Option<ViewerSource>,
    generation: Generation,
    /// Ready-to-render Markdown text. Held here (not inline in
    /// `ViewerStatus::Ready`) so the enum stays `Clone` for testing.
    markdown: Option<String>,
    image: Option<ImageReady>,
    return_focus: ReturnFocus,
    /// Zero-based first visible wrapped row of the Markdown snapshot.
    /// Ignored for image sources.
    markdown_scroll: u16,
    /// Ratatui-image scale key (`+ - 0`). Ignored for Markdown.
    image_scale: i16,

    // --- §19.11 addendum: interaction state ---
    /// Text-area rect captured on the last `render_into_pane` call, in
    /// absolute frame coordinates. Used by [`Self::on_mouse`] to
    /// translate mouse hits into text cells. `None` before the first
    /// render — all mouse events are dropped until then.
    text_area: Option<Rect>,
    /// Right-edge scrollbar rect captured on the last render. `None`
    /// when there's nothing to scroll (content fits) or when the
    /// snapshot is an image.
    scrollbar_rect: Option<Rect>,
    /// Total wrapped-row count of the current Markdown at
    /// `text_area.width`, refreshed every render. Zero for image /
    /// loading / error states. Drives the scrollbar thumb and clamps
    /// [`Self::scroll_markdown`].
    content_lines: u16,
    /// Local text selection over the rendered viewport
    /// (§19.11 addendum). Cells are stored relative to `text_area`
    /// with `.row` in the visible viewport (NOT source-line).
    selection: SelectionState,
    /// Per-row snapshot of the rendered viewport, captured out of the
    /// ratatui `Buffer` right after `Paragraph` renders. Used by
    /// [`Self::copy_selection`] outside the draw pass — the buffer is
    /// only borrowable inside `render`.
    rendered_lines: Vec<String>,
    /// Set while the user holds Left on the scrollbar column. Contains
    /// the `text_area.y` at drag start so mouse `Drag` events can
    /// compute the new scroll position by row ratio.
    scrollbar_drag: bool,
    /// `[×]` close-button rect captured on the last render (top-right
    /// corner of the viewer border). `None` when the pane was too
    /// narrow to fit the 3-cell affordance. Consulted by
    /// [`Self::on_mouse`] on `Down(Left)` before selection / scrollbar
    /// hits so users can always dismiss the viewer with the mouse.
    close_button_rect: Option<Rect>,
    /// One-shot flag set when the user clicks `[×]`. The App polls
    /// [`Self::take_close_request`] each frame; on `true` it calls
    /// `close_viewer_overlay` (which owns focus restoration, so we
    /// don't try to duplicate that logic here).
    close_requested: bool,
}

impl Default for ViewerOverlayState {
    fn default() -> Self {
        Self {
            status: ViewerStatus::Closed,
            snapshot: None,
            generation: Generation(0),
            markdown: None,
            image: None,
            return_focus: None,
            markdown_scroll: 0,
            image_scale: 0,
            text_area: None,
            scrollbar_rect: None,
            content_lines: 0,
            selection: SelectionState::default(),
            rendered_lines: Vec::new(),
            scrollbar_drag: false,
            close_button_rect: None,
            close_requested: false,
        }
    }
}

impl ViewerOverlayState {
    /// True while the overlay owns input focus.
    pub fn is_open(&self) -> bool {
        !matches!(self.status, ViewerStatus::Closed)
    }

    pub fn status(&self) -> &ViewerStatus {
        &self.status
    }

    pub fn snapshot(&self) -> Option<&ViewerSource> {
        self.snapshot.as_ref()
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn return_focus(&self) -> ReturnFocus {
        self.return_focus
    }

    pub fn markdown(&self) -> Option<&str> {
        self.markdown.as_deref()
    }

    pub fn image(&self) -> Option<&ImageReady> {
        self.image.as_ref()
    }

    pub fn markdown_scroll(&self) -> u16 {
        self.markdown_scroll
    }

    pub fn image_scale(&self) -> i16 {
        self.image_scale
    }

    pub fn open_snapshot(&mut self, source: ViewerSource, return_focus: ReturnFocus) -> Generation {
        self.generation = Generation(self.generation.0.wrapping_add(1));
        self.status = ViewerStatus::Loading;
        self.snapshot = Some(source);
        self.markdown = None;
        self.image = None;
        self.markdown_scroll = 0;
        self.image_scale = 0;
        self.return_focus = return_focus;
        self.selection.clear();
        self.rendered_lines.clear();
        self.content_lines = 0;
        self.text_area = None;
        self.scrollbar_rect = None;
        self.scrollbar_drag = false;
        self.close_button_rect = None;
        self.close_requested = false;
        self.generation
    }

    /// Apply a worker completion. Returns `true` when the payload was
    /// accepted; `false` when it referred to a stale snapshot (older
    /// generation, different path, or overlay already closed).
    pub fn apply_completion(&mut self, completion: ViewerCompletion) -> bool {
        if completion.generation != self.generation {
            return false;
        }
        let Some(source) = &self.snapshot else {
            return false;
        };
        if completion.path != source.path {
            return false;
        }
        match completion.payload {
            ViewerPayload::Markdown(text) => {
                if source.kind != ViewerKind::Markdown {
                    return false;
                }
                self.markdown = Some(text);
                self.status = ViewerStatus::Ready;
            }
            ViewerPayload::Image(image) => {
                if source.kind != ViewerKind::Image {
                    return false;
                }
                self.image = Some(image);
                self.status = ViewerStatus::Ready;
            }
            ViewerPayload::Error(msg) => {
                self.status = ViewerStatus::Error(msg);
            }
        }
        true
    }

    /// Close the overlay. Drops payload, invalidates the generation
    /// (so any late worker completion is ignored), and yields the
    /// return-focus for the caller to restore.
    pub fn close(&mut self) -> ReturnFocus {
        let focus = self.return_focus.take();
        self.status = ViewerStatus::Closed;
        self.snapshot = None;
        self.markdown = None;
        self.image = None;
        self.markdown_scroll = 0;
        self.image_scale = 0;
        self.selection.clear();
        self.rendered_lines.clear();
        self.content_lines = 0;
        self.text_area = None;
        self.scrollbar_rect = None;
        self.scrollbar_drag = false;
        self.close_button_rect = None;
        self.close_requested = false;
        // Bump generation on close too — a completion that races the
        // close is definitively stale.
        self.generation = Generation(self.generation.0.wrapping_add(1));
        focus
    }

    /// Latest active-Yazi selection is NOT propagated into an open
    /// snapshot (§19.7.20). Callers update their own `last_yazi_selection`
    /// cache; the state is unaffected.
    pub fn ignore_background_selection(&self, _incoming: &Path) {
        // Intentionally empty. Documented no-op so integration code can
        // call this at the right point without threading tests through
        // side-channels.
    }

    /// One-shot poll: returns `true` and clears the flag when the user
    /// clicked `[×]` on the last-drawn viewer. Consulted by the App
    /// after routing a mouse event through [`Self::on_mouse`] so it
    /// can call `close_viewer_overlay` (which owns focus restoration).
    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// Maximum permitted `markdown_scroll` value at the current
    /// viewport size. Zero when the content fits, or when the
    /// snapshot is Image / Loading / Error (nothing to scroll).
    /// Refreshed on every `render_into_pane` call.
    pub fn max_scroll(&self) -> u16 {
        let viewport = self.text_area.map(|r| r.height).unwrap_or(0);
        self.content_lines.saturating_sub(viewport)
    }

    /// Adjust Markdown scroll by `delta` rows, clamped against
    /// [`Self::max_scroll`]. No-op when the snapshot is not Markdown.
    /// Positive `delta` scrolls down; negative up. §19.11 addendum.
    pub fn scroll_markdown(&mut self, delta: i32) {
        if !matches!(
            self.snapshot.as_ref().map(|s| s.kind),
            Some(ViewerKind::Markdown),
        ) {
            return;
        }
        let max = i32::from(self.max_scroll());
        let current = i32::from(self.markdown_scroll);
        let clamped = current.saturating_add(delta).clamp(0, max);
        self.markdown_scroll = clamped as u16;
        // Any scroll invalidates selection cell coordinates (they were
        // in the previous viewport). Simpler to drop than to re-project.
        self.selection.clear();
    }

    /// Test-only setter that seeds content-line count without going
    /// through the render path. Exists so unit tests can exercise
    /// [`Self::scroll_markdown`] clamp behaviour without spinning up
    /// a ratatui Buffer.
    #[cfg(test)]
    pub fn set_content_metrics_for_test(&mut self, content_lines: u16, viewport_height: u16) {
        self.content_lines = content_lines;
        self.text_area = Some(Rect {
            x: 0,
            y: 0,
            width: 40,
            height: viewport_height,
        });
    }

    /// Reset image scale to 0 (Fit). No-op when snapshot is not Image.
    pub fn reset_image_scale(&mut self) {
        if matches!(
            self.snapshot.as_ref().map(|s| s.kind),
            Some(ViewerKind::Image),
        ) {
            self.image_scale = 0;
        }
    }

    /// Nudge image scale by `delta`, clamped to `[-4, 8]`. No-op when
    /// snapshot is not Image.
    pub fn nudge_image_scale(&mut self, delta: i16) {
        if matches!(
            self.snapshot.as_ref().map(|s| s.kind),
            Some(ViewerKind::Image),
        ) {
            self.image_scale = (self.image_scale + delta).clamp(-4, 8);
        }
    }

    /// Dispatch a mouse event delivered by the App while the viewer is
    /// open (§19.11 addendum). Returns `true` when the event was
    /// consumed; the App swallows it either way to keep the panes
    /// underneath quiet.
    ///
    /// Behavior mirrors the Quick Look policy (§19.14.2):
    /// - `Down(Left)` on the scrollbar column starts a thumb drag.
    /// - `Down(Left)` anywhere else in the text area begins a
    ///   local text selection.
    /// - `Drag(Left)` extends selection or moves the scroll thumb.
    /// - `Up(Left)` commits selection + copies to the clipboard, or
    ///   releases the scrollbar.
    /// - `Down(Right)` copies any active selection (read-only zone,
    ///   never paste — mirrors §19.14 QuickLook policy).
    /// - `ScrollUp` / `ScrollDown` step the scroll by 3 wrapped rows.
    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        // The close button exists in Loading / Ready / Error states,
        // before a Markdown `text_area` necessarily exists. Test it
        // before the text-area guard so `[×]` always works.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .close_button_rect
                .is_some_and(|r| point_in_rect(ev.column, ev.row, r))
        {
            self.close_requested = true;
            self.selection.clear();
            return true;
        }

        let Some(text_area) = self.text_area else {
            return false;
        };
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        match ev.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_markdown(-3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_markdown(3);
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.point_on_scrollbar(ev.column, ev.row) {
                    self.scrollbar_drag = true;
                    self.selection.clear();
                    self.set_scroll_from_scrollbar_row(ev.row, text_area);
                    return true;
                }
                if let Some(cell) = cell_in_area(ev.column, ev.row, text_area) {
                    if shift {
                        self.selection.shift_extend(cell);
                    } else {
                        self.selection.begin(cell, Instant::now());
                    }
                    true
                } else {
                    // Border / title strip / hint area inside pane
                    // rect. Still consume so the underlying pane
                    // doesn't see the event.
                    self.selection.clear();
                    true
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_drag {
                    self.set_scroll_from_scrollbar_row(ev.row, text_area);
                    return true;
                }
                if self.selection.is_active() {
                    let cell = cell_in_area_clamped(ev.column, ev.row, text_area);
                    self.selection.extend(cell);
                    return true;
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.scrollbar_drag {
                    self.scrollbar_drag = false;
                    return true;
                }
                if self.selection.is_active() {
                    self.selection.commit();
                    self.copy_selection();
                    return true;
                }
                false
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // §19.11 addendum: viewer is a read-only surface.
                // Right-click = copy any active selection, never paste.
                if self.selection.is_active() {
                    self.copy_selection();
                    self.selection.clear();
                }
                true
            }
            _ => false,
        }
    }

    /// True when the currently-drawn scrollbar exists and covers
    /// `(col, row)`. Used to decide whether a `Down(Left)` starts a
    /// thumb drag or a text selection.
    fn point_on_scrollbar(&self, col: u16, row: u16) -> bool {
        self.scrollbar_rect
            .is_some_and(|r| point_in_rect(col, row, r))
    }

    /// Map a scrollbar-column mouse row to a scroll position by
    /// linear interpolation (top row → scroll 0; bottom row →
    /// max_scroll). Bail out silently when there's nothing to scroll.
    fn set_scroll_from_scrollbar_row(&mut self, row: u16, text_area: Rect) {
        let max = self.max_scroll();
        if max == 0 || text_area.height == 0 {
            return;
        }
        let clamped = row.clamp(
            text_area.y,
            text_area
                .y
                .saturating_add(text_area.height.saturating_sub(1)),
        );
        let rel = u32::from(clamped - text_area.y);
        let span = u32::from(text_area.height.saturating_sub(1)).max(1);
        let new_scroll = ((rel * u32::from(max) + span / 2) / span) as u16;
        self.markdown_scroll = new_scroll.min(max);
        // The scrollbar drag deliberately does NOT clear the current
        // selection — dragging the thumb should not lose a highlight
        // the user made just before. But cells now point at a
        // different viewport row; simplest is to drop it. Matches
        // [`Self::scroll_markdown`].
        self.selection.clear();
    }

    /// Extract the currently-selected text from `rendered_lines` and
    /// push it to the system clipboard. No-op on empty selection or
    /// missing snapshot.
    pub fn copy_selection(&self) {
        if !self.selection.is_active() {
            return;
        }
        let Some((start, end)) = self.selection.char_range() else {
            return;
        };
        let mut out = String::new();
        let last_row = end.row as usize;
        for row in (start.row as usize)..=last_row {
            let Some(line) = self.rendered_lines.get(row) else {
                break;
            };
            let (col_start, col_end) = if start.row == end.row {
                (start.col, end.col)
            } else if row == start.row as usize {
                (start.col, u16::MAX)
            } else if row == last_row {
                (0, end.col)
            } else {
                (0, u16::MAX)
            };
            let slice = extract_columns(line, col_start, col_end);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&slice);
        }
        if out.trim().is_empty() {
            return;
        }
        if let Ok(mut clip) = arboard::Clipboard::new() {
            let _ = clip.set_text(out);
        }
    }

    // --- Accessors used by the App / integration tests ---

    /// Content lines currently reported by the last render. Zero for
    /// non-Markdown / loading states.
    pub fn content_lines(&self) -> u16 {
        self.content_lines
    }

    /// Absolute-coordinate rect of the last-rendered text area, or
    /// `None` if the viewer hasn't been drawn yet.
    pub fn text_area(&self) -> Option<Rect> {
        self.text_area
    }

    /// True while a scrollbar thumb drag is in progress (Left held).
    /// The App consults this so click sequences that cross out of the
    /// viewer rect still route back here until the button releases.
    pub fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag
    }

    /// True while a text selection drag is in progress (Left held).
    /// Same routing rationale as [`Self::scrollbar_dragging`].
    pub fn selection_active(&self) -> bool {
        self.selection.is_active()
    }
}

/// Blocking Markdown reader used by the tokio worker (§19.11.3). Enforces
/// [`MARKDOWN_MAX_BYTES`] and rejects non-UTF-8 content — the overlay
/// promises rendered CommonMark, not raw bytes.
pub fn load_markdown_blocking(path: &Path) -> ViewerPayload {
    match read_markdown_bytes(path, MARKDOWN_MAX_BYTES) {
        Ok(text) => ViewerPayload::Markdown(text),
        Err(err) => ViewerPayload::Error(err),
    }
}

/// Blocking image reader used by the tokio worker (§19.11.3). Enforces
/// [`IMAGE_MAX_BYTES`], only accepts the formats declared in
/// [`IMAGE_EXTS`], and returns a decoded [`image::DynamicImage`]
/// (animation frames are collapsed to the first frame).
pub fn load_image_blocking(path: &Path) -> ViewerPayload {
    match read_image_dyn(path, IMAGE_MAX_BYTES) {
        Ok(image) => ViewerPayload::Image(ImageReady { image }),
        Err(err) => ViewerPayload::Error(err),
    }
}

fn read_image_dyn(path: &Path, cap: u64) -> Result<image::DynamicImage, String> {
    let metadata = std::fs::metadata(path).map_err(|e| io_err(&e))?;
    if !metadata.is_file() {
        return Err("not a regular file".into());
    }
    if metadata.len() > cap {
        return Err(format!(
            "file exceeds {} MiB image limit",
            cap / 1024 / 1024
        ));
    }
    image::ImageReader::open(path)
        .map_err(|e| io_err(&e))?
        .with_guessed_format()
        .map_err(|e| io_err(&e))?
        .decode()
        .map_err(|e| e.to_string())
}

fn read_markdown_bytes(path: &Path, cap: u64) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|e| io_err(&e))?;
    if !metadata.is_file() {
        return Err("not a regular file".into());
    }
    if metadata.len() > cap {
        return Err(format!(
            "file exceeds {} MiB Markdown limit",
            cap / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| io_err(&e))?;
    String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".to_string())
}

fn io_err(e: &io::Error) -> String {
    e.to_string()
}

/// Render the viewer into the left-column pane rect (§19.11 addendum:
/// the viewer is now a true fullscreen takeover of the yazi pane, not
/// a floating modal). Owns no I/O — parses the stored Markdown source
/// on the render thread, which is bounded to the 8 MiB cap.
///
/// Takes `state` by `&mut` so it can persist per-frame layout hints
/// (`text_area`, `scrollbar_rect`, `content_lines`) plus a snapshot of
/// the rendered rows for [`ViewerOverlayState::copy_selection`].
pub fn render_into_pane(
    state: &mut ViewerOverlayState,
    pane_rect: Rect,
    buf: &mut Buffer,
    picker: Option<&ratatui_image::picker::Picker>,
) {
    if !state.is_open() {
        return;
    }
    let title = build_title(state);
    let block = Block::default()
        .title(Line::styled(
            title,
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(pane_rect);
    // Clear pane cells so any previously-rendered yazi glyphs don't
    // bleed through where the block skips characters (e.g. gaps
    // between paragraphs).
    ratatui::widgets::Clear.render(pane_rect, buf);
    block.render(pane_rect, buf);

    // Close affordance lives on the tab strip's `×` (files group is now
    // an Open policy; viewer tabs are closable). We deliberately do NOT
    // paint an in-viewer [×] anymore.
    state.close_button_rect = None;

    // Reset frame-scoped state before deciding how to fill inner.
    state.text_area = None;
    state.scrollbar_rect = None;
    state.content_lines = 0;
    state.rendered_lines.clear();

    // Split up the immutable borrows before we hand `state` mutably to
    // `render_markdown` (which persists geometry back onto the state).
    let status = state.status().clone();
    let kind = state.snapshot().map(|s| s.kind);
    match (status, kind) {
        (ViewerStatus::Loading, _) => render_message(inner, buf, "Loading…"),
        (ViewerStatus::Error(msg), _) => render_message(inner, buf, &msg),
        (ViewerStatus::Ready, Some(ViewerKind::Markdown)) => {
            render_markdown(state, inner, buf);
        }
        (ViewerStatus::Ready, Some(ViewerKind::Image)) => {
            state.text_area = Some(inner);
            render_image(state, inner, buf, picker);
        }
        _ => {}
    }
}

/// Legacy name kept as a thin wrapper so external callers (currently
/// none outside app.rs — but tests reference it) compile against the
/// new signature during the transition.
pub fn render_overlay(
    state: &mut ViewerOverlayState,
    bounds: Rect,
    buf: &mut Buffer,
    picker: Option<&ratatui_image::picker::Picker>,
) {
    render_into_pane(state, bounds, buf, picker);
}

/// Renders Markdown into `inner`, reserving the right-most column for
/// a `ratatui::Scrollbar`. Captures layout hints on `state` for the
/// mouse handler + `copy_selection`.
fn render_markdown(state: &mut ViewerOverlayState, inner: Rect, buf: &mut Buffer) {
    if inner.width < 2 || inner.height == 0 {
        return;
    }
    let source = state.markdown().unwrap_or("").to_string();
    let text = tui_markdown::from_str(&source);

    // Reserve the right-most column for the scrollbar. We split
    // upfront so `content_lines` counting matches the paragraph width
    // exactly (else the thumb would drift by 1 col of wrap).
    let text_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width.saturating_sub(1),
        height: inner.height,
    };
    let scrollbar_col = Rect {
        x: inner.x.saturating_add(text_area.width),
        y: inner.y,
        width: 1,
        height: inner.height,
    };

    let content_lines = wrapped_line_count(&text, text_area.width);
    state.content_lines = content_lines;
    state.text_area = Some(text_area);

    // Clamp scroll defensively — the content may have shrunk between
    // renders (e.g. after a resize). Compute against post-count max.
    let max_scroll = content_lines.saturating_sub(text_area.height);
    if state.markdown_scroll > max_scroll {
        state.markdown_scroll = max_scroll;
    }
    let scroll = state.markdown_scroll;

    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(text_area, buf);

    // Snapshot the rendered rows out of the buffer so the mouse-up
    // copy path can produce exactly what the user saw. Cheap on a
    // ~35% column: ≤ text_area.width * text_area.height Cell reads.
    state.rendered_lines = capture_rendered_rows(buf, text_area);

    // Selection reverse-video overlay. Painted AFTER the paragraph
    // so it wins the style merge, matching `PtyPane`'s convention.
    if state.selection.is_active() {
        let cols = text_area.width;
        for row in 0..text_area.height {
            for col in 0..text_area.width {
                if state.selection.contains(row, col, cols) {
                    let target = &mut buf[(text_area.x + col, text_area.y + row)];
                    let style = target.style().add_modifier(Modifier::REVERSED);
                    target.set_style(style);
                }
            }
        }
    }

    // Scrollbar. Only draw when there's something to scroll — an
    // idle thumb on a short document looks broken.
    let max_scroll = content_lines.saturating_sub(text_area.height);
    if max_scroll > 0 && scrollbar_col.width > 0 {
        let mut sb_state = ScrollbarState::new(usize::from(content_lines))
            .position(usize::from(scroll))
            .viewport_content_length(usize::from(text_area.height));
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .render(scrollbar_col, buf, &mut sb_state);
        state.scrollbar_rect = Some(scrollbar_col);
    }
}

/// Read `area`'s cells out of the ratatui buffer and turn each row
/// into a flat display string (spacer cells for wide chars contribute
/// their empty symbol, so widths line up column-for-column).
fn capture_rendered_rows(buf: &Buffer, area: Rect) -> Vec<String> {
    let mut rows = Vec::with_capacity(area.height as usize);
    for row in 0..area.height {
        let mut line = String::with_capacity(area.width as usize);
        for col in 0..area.width {
            let x = area.x + col;
            let y = area.y + row;
            let sym = buf
                .cell(ratatui::layout::Position::new(x, y))
                .map(|c| c.symbol())
                .unwrap_or("");
            line.push_str(sym);
        }
        rows.push(line);
    }
    rows
}

/// Sum the wrapped-row count of a `Text` at `width`. Approximates
/// ratatui's word-wrap by ceil-dividing each line's display width;
/// good enough for scrollbar sizing and PgDn behaviour on Markdown
/// (accuracy off by ≤1 row per wrapped source line).
fn wrapped_line_count(text: &Text, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let width_u32 = u32::from(width);
    let mut total: u32 = 0;
    for line in &text.lines {
        let mut line_width: u32 = 0;
        for span in &line.spans {
            line_width =
                line_width.saturating_add(UnicodeWidthStr::width(span.content.as_ref()) as u32);
        }
        let rows = if line_width == 0 {
            1
        } else {
            line_width.div_ceil(width_u32)
        };
        total = total.saturating_add(rows);
    }
    total.min(u32::from(u16::MAX)) as u16
}

/// Convert an absolute-coordinate mouse hit into a text-area cell,
/// or `None` when the point misses. Cells are 0-indexed from
/// `text_area.x` / `.y`, matching what `SelectionState` expects.
fn cell_in_area(col: u16, row: u16, text_area: Rect) -> Option<SelCell> {
    if !point_in_rect(col, row, text_area) {
        return None;
    }
    Some(SelCell {
        row: row - text_area.y,
        col: col - text_area.x,
    })
}

/// Same as [`cell_in_area`] but clamps overshoots into the rect so
/// a drag past the border still updates the selection cursor.
fn cell_in_area_clamped(col: u16, row: u16, text_area: Rect) -> SelCell {
    let x = col.clamp(
        text_area.x,
        text_area
            .x
            .saturating_add(text_area.width.saturating_sub(1)),
    );
    let y = row.clamp(
        text_area.y,
        text_area
            .y
            .saturating_add(text_area.height.saturating_sub(1)),
    );
    SelCell {
        row: y - text_area.y,
        col: x - text_area.x,
    }
}

/// Slice `line` (a captured display-row) to the display-column range
/// `[start_col, end_col]` inclusive. Handles CJK / emoji by summing
/// `unicode_width` per char.
fn extract_columns(line: &str, start_col: u16, end_col: u16) -> String {
    if end_col < start_col {
        return String::new();
    }
    let mut out = String::new();
    let mut col: u16 = 0;
    for c in line.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0) as u16;
        if col > end_col {
            break;
        }
        if col >= start_col {
            out.push(c);
        }
        col = col.saturating_add(w.max(1));
    }
    out.trim_end().to_string()
}

/// Inclusive-left, exclusive-right rectangle hit test. Duplicated
/// here to avoid taking a dependency on `pty_pane`; matches its
/// semantics.
fn point_in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn render_image(
    state: &ViewerOverlayState,
    inner: Rect,
    buf: &mut Buffer,
    picker: Option<&ratatui_image::picker::Picker>,
) {
    let Some(picker) = picker else {
        render_message(
            inner,
            buf,
            "Graphics protocol unavailable — install a compatible terminal for image preview.",
        );
        return;
    };
    let Some(image) = state.image().map(|r| &r.image) else {
        return;
    };
    let target = image_target_size(inner, state.image_scale());
    match picker.new_protocol(image.clone(), target, ratatui_image::Resize::Fit(None)) {
        Ok(proto) => {
            ratatui_image::Image::new(&proto)
                .allow_clipping(true)
                .render(inner, buf);
        }
        Err(err) => render_message(inner, buf, &format!("image protocol error: {err}")),
    }
}

fn image_target_size(inner: Rect, scale: i16) -> ratatui::layout::Size {
    // Scale steps double / halve area (roughly). Keep the mapping small
    // so users can walk +/- without overshooting the block.
    let factor = match scale {
        -4 => 1.0 / 4.0,
        -3 => 1.0 / 3.0,
        -2 => 0.5,
        -1 => 0.75,
        0 => 1.0,
        1 => 1.25,
        2 => 1.5,
        3 => 2.0,
        4 => 2.5,
        5 => 3.0,
        _ => 3.5,
    };
    let w = ((f32::from(inner.width) * factor).round() as u16).max(1);
    let h = ((f32::from(inner.height) * factor).round() as u16).max(1);
    ratatui::layout::Size::new(w, h)
}

fn build_title(state: &ViewerOverlayState) -> String {
    let name = state
        .snapshot()
        .and_then(|s| s.path.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("viewer");
    let kind = match state.snapshot().map(|s| s.kind) {
        Some(ViewerKind::Markdown) => "Markdown",
        Some(ViewerKind::Image) => "Image",
        None => "viewer",
    };
    let badge = match state.status() {
        ViewerStatus::Closed => "closed",
        ViewerStatus::Loading => "loading",
        ViewerStatus::Ready => "ready",
        ViewerStatus::Error(_) => "error",
    };
    format!(" 📖 {name} · {kind} · {badge} ")
}

fn render_message(area: Rect, buf: &mut Buffer, msg: &str) {
    Paragraph::new(msg)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

/// Lightweight pane provider that occupies a tab slot in the files group.
/// The actual rendering is done by `render_into_pane` using the matching
/// `ViewerOverlayState` in `App::viewers`. This struct only carries the
/// stable id and the tab title (the file's basename).
pub struct ViewerPane {
    id: PaneId,
    title: String,
}

impl ViewerPane {
    pub fn new(title: String) -> Self {
        Self {
            id: PaneId::next(),
            title,
        }
    }
}

impl PaneProvider for ViewerPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }

    fn render(&mut self, _area: Rect, _buf: &mut Buffer, _ctx: &PaneRenderCtx) -> RenderOutcome {
        // App renders viewer content directly via render_into_pane.
        RenderOutcome::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regular(len: u64) -> SourceMeta {
        SourceMeta {
            is_regular_file: true,
            len,
        }
    }

    mod classify_source {
        use super::*;

        #[test]
        fn markdown_extensions_are_case_insensitive() {
            for name in ["notes.md", "Notes.MD", "readme.markdown", "R.Markdown"] {
                let src = classify_source(Path::new(name), regular(1024))
                    .expect("classify")
                    .expect("supported");
                assert_eq!(
                    src.kind,
                    ViewerKind::Markdown,
                    "expected Markdown for {name}"
                );
            }
        }

        #[test]
        fn image_extensions_accept_documented_formats() {
            for name in [
                "logo.png",
                "photo.jpg",
                "photo.jpeg",
                "anim.gif",
                "art.webp",
                "ico.bmp",
            ] {
                let src = classify_source(Path::new(name), regular(1024))
                    .expect("classify")
                    .expect("supported");
                assert_eq!(src.kind, ViewerKind::Image, "expected Image for {name}");
            }
        }

        #[test]
        fn svg_and_unknown_extensions_return_none() {
            for name in ["diagram.svg", "Cargo.toml", "readme.rst", "notype"] {
                let outcome = classify_source(Path::new(name), regular(1024)).expect("classify");
                assert!(outcome.is_none(), "expected None for {name}");
            }
        }

        #[test]
        fn non_regular_file_is_rejected_before_size_check() {
            let err = classify_source(
                Path::new("notes.md"),
                SourceMeta {
                    is_regular_file: false,
                    len: u64::MAX,
                },
            )
            .expect_err("non-regular file rejected");
            assert_eq!(err, ClassifyError::NotRegularFile);
        }

        #[test]
        fn markdown_over_cap_reports_size_and_cap() {
            let err = classify_source(Path::new("big.md"), regular(MARKDOWN_MAX_BYTES + 1))
                .expect_err("size cap");
            assert_eq!(
                err,
                ClassifyError::TooLarge {
                    size: MARKDOWN_MAX_BYTES + 1,
                    cap: MARKDOWN_MAX_BYTES,
                }
            );
        }

        #[test]
        fn image_at_cap_is_admissible() {
            let src = classify_source(Path::new("frame.png"), regular(IMAGE_MAX_BYTES))
                .expect("classify")
                .expect("supported");
            assert_eq!(src.kind, ViewerKind::Image);
        }

        #[test]
        fn image_over_cap_reports_size_and_cap() {
            let err = classify_source(Path::new("big.png"), regular(IMAGE_MAX_BYTES + 1))
                .expect_err("size cap");
            assert_eq!(
                err,
                ClassifyError::TooLarge {
                    size: IMAGE_MAX_BYTES + 1,
                    cap: IMAGE_MAX_BYTES,
                }
            );
        }
    }

    mod overlay_rect {
        use super::*;

        #[test]
        fn rejects_bounds_below_minimum() {
            assert_eq!(
                overlay_rect(Rect::new(0, 0, OVERLAY_MIN_COLS - 1, OVERLAY_MIN_ROWS)),
                None
            );
            assert_eq!(
                overlay_rect(Rect::new(0, 0, OVERLAY_MIN_COLS, OVERLAY_MIN_ROWS - 1)),
                None
            );
        }

        #[test]
        fn accepts_bounds_at_minimum_and_never_falls_below_floor() {
            let rect = overlay_rect(Rect::new(0, 0, OVERLAY_MIN_COLS, OVERLAY_MIN_ROWS))
                .expect("minimum bounds accepted");
            assert!(rect.width >= OVERLAY_MIN_COLS);
            assert!(rect.height >= OVERLAY_MIN_ROWS);
        }

        #[test]
        fn typical_120x34_takes_ninety_percent_and_centers() {
            let bounds = Rect::new(0, 0, 120, 34);
            let rect = overlay_rect(bounds).expect("fits");
            // 90% of 120 = 108 cols; 90% of 34 = 30 rows.
            assert_eq!(rect.width, 108);
            assert_eq!(rect.height, 30);
            assert_eq!(rect.x, (bounds.width - rect.width) / 2);
            assert_eq!(rect.y, (bounds.height - rect.height) / 2);
        }

        #[test]
        fn overlay_respects_the_two_cell_outer_margin() {
            let bounds = Rect::new(0, 0, 120, 34);
            let rect = overlay_rect(bounds).expect("fits");
            assert!(rect.x >= OVERLAY_MARGIN);
            assert!(rect.y >= OVERLAY_MARGIN);
            assert!(bounds.x + bounds.width - (rect.x + rect.width) >= OVERLAY_MARGIN);
            assert!(bounds.y + bounds.height - (rect.y + rect.height) >= OVERLAY_MARGIN);
        }

        #[test]
        fn non_zero_origin_bounds_are_respected() {
            let bounds = Rect::new(3, 5, 100, 40);
            let rect = overlay_rect(bounds).expect("fits");
            assert!(rect.x >= bounds.x);
            assert!(rect.y >= bounds.y);
            assert!(rect.x + rect.width <= bounds.x + bounds.width);
            assert!(rect.y + rect.height <= bounds.y + bounds.height);
        }
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    use std::path::PathBuf;

    fn src(name: &str, kind: ViewerKind) -> ViewerSource {
        ViewerSource {
            path: PathBuf::from(name),
            kind,
        }
    }

    fn markdown_completion(snap_gen: Generation, name: &str, body: &str) -> ViewerCompletion {
        ViewerCompletion {
            pane_id: PaneId(0),
            generation: snap_gen,
            path: PathBuf::from(name),
            payload: ViewerPayload::Markdown(body.to_owned()),
        }
    }

    #[test]
    fn default_state_is_closed() {
        let state = ViewerOverlayState::default();
        assert!(!state.is_open());
        assert!(matches!(state.status(), ViewerStatus::Closed));
        assert_eq!(state.generation(), Generation(0));
    }

    #[test]
    fn opening_marks_loading_and_bumps_generation() {
        let mut state = ViewerOverlayState::default();
        let gen1 = state.open_snapshot(src("a.md", ViewerKind::Markdown), Some(PaneId(7)));
        assert!(state.is_open());
        assert!(matches!(state.status(), ViewerStatus::Loading));
        assert_eq!(gen1, Generation(1));
        assert_eq!(state.return_focus(), Some(PaneId(7)));

        let gen2 = state.open_snapshot(src("b.md", ViewerKind::Markdown), None);
        assert_eq!(gen2, Generation(2));
        assert_eq!(state.snapshot().unwrap().path, PathBuf::from("b.md"));
    }

    #[test]
    fn matching_completion_promotes_to_ready() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        assert!(state.apply_completion(markdown_completion(snap_gen, "a.md", "# hello")));
        assert!(matches!(state.status(), ViewerStatus::Ready));
        assert_eq!(state.markdown(), Some("# hello"));
    }

    #[test]
    fn stale_generation_is_rejected() {
        let mut state = ViewerOverlayState::default();
        let old_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        // Second open bumps generation; the first worker returns late.
        state.open_snapshot(src("b.md", ViewerKind::Markdown), None);
        assert!(!state.apply_completion(markdown_completion(old_gen, "a.md", "old")));
        assert!(matches!(state.status(), ViewerStatus::Loading));
        assert_eq!(state.markdown(), None);
    }

    #[test]
    fn mismatched_path_is_rejected() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        assert!(!state.apply_completion(markdown_completion(snap_gen, "other.md", "nope")));
        assert!(matches!(state.status(), ViewerStatus::Loading));
    }

    #[test]
    fn wrong_kind_completion_is_rejected() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        let bogus = ViewerCompletion {
            pane_id: PaneId(0),
            generation: snap_gen,
            path: PathBuf::from("a.md"),
            payload: ViewerPayload::Image(ImageReady {
                image: image::DynamicImage::new_rgba8(1, 1),
            }),
        };
        assert!(!state.apply_completion(bogus));
        assert!(matches!(state.status(), ViewerStatus::Loading));
    }

    #[test]
    fn error_completion_surfaces_but_keeps_overlay_open() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        let err = ViewerCompletion {
            pane_id: PaneId(0),
            generation: snap_gen,
            path: PathBuf::from("a.md"),
            payload: ViewerPayload::Error("permission denied".into()),
        };
        assert!(state.apply_completion(err));
        assert!(state.is_open());
        assert!(matches!(state.status(), ViewerStatus::Error(msg) if msg == "permission denied"));
    }

    #[test]
    fn close_returns_focus_and_invalidates_late_completion() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), Some(PaneId(42)));
        assert_eq!(state.close(), Some(PaneId(42)));
        assert!(!state.is_open());
        assert!(!state.apply_completion(markdown_completion(snap_gen, "a.md", "late")));
    }

    #[test]
    fn background_selection_does_not_replace_open_snapshot() {
        let mut state = ViewerOverlayState::default();
        let snap_gen = state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.ignore_background_selection(std::path::Path::new("b.md"));
        assert_eq!(state.snapshot().unwrap().path, PathBuf::from("a.md"));
        assert_eq!(state.generation(), snap_gen);
    }

    #[test]
    fn scroll_markdown_clamps_to_max_and_ignores_image() {
        // Fresh Markdown snapshot: seed content metrics so max_scroll
        // is well-defined without going through a real render.
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(13, 10); // 13 rows, 10-row viewport → max = 3
        state.scroll_markdown(5);
        assert_eq!(state.markdown_scroll(), 3);
        state.scroll_markdown(-100);
        assert_eq!(state.markdown_scroll(), 0);

        // Image snapshots ignore scroll_markdown entirely.
        state.open_snapshot(src("logo.png", ViewerKind::Image), None);
        state.scroll_markdown(5);
        assert_eq!(state.markdown_scroll(), 0);
    }

    #[test]
    fn image_scale_clamps_and_ignores_markdown() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("logo.png", ViewerKind::Image), None);
        for _ in 0..20 {
            state.nudge_image_scale(1);
        }
        assert_eq!(state.image_scale(), 8);
        state.reset_image_scale();
        assert_eq!(state.image_scale(), 0);
        for _ in 0..20 {
            state.nudge_image_scale(-1);
        }
        assert_eq!(state.image_scale(), -4);

        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.nudge_image_scale(3);
        assert_eq!(state.image_scale(), 0);
    }

    // --- §19.11 addendum: mouse handling for the pane-inside viewer ---

    fn mev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_scrolls_by_three_wrapped_rows() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(50, 10); // max = 40

        assert!(state.on_mouse(mev(MouseEventKind::ScrollDown, 5, 5)));
        assert_eq!(state.markdown_scroll(), 3);
        assert!(state.on_mouse(mev(MouseEventKind::ScrollDown, 5, 5)));
        assert_eq!(state.markdown_scroll(), 6);
        assert!(state.on_mouse(mev(MouseEventKind::ScrollUp, 5, 5)));
        assert_eq!(state.markdown_scroll(), 3);
    }

    #[test]
    fn wheel_clamps_at_bounds() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(12, 10); // max = 2

        for _ in 0..10 {
            state.on_mouse(mev(MouseEventKind::ScrollDown, 5, 5));
        }
        assert_eq!(state.markdown_scroll(), 2);
        for _ in 0..10 {
            state.on_mouse(mev(MouseEventKind::ScrollUp, 5, 5));
        }
        assert_eq!(state.markdown_scroll(), 0);
    }

    #[test]
    fn left_down_in_text_area_begins_selection() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(20, 10);
        // set_content_metrics_for_test seeds text_area = (0,0,40,10);
        // click at (col=5, row=3) is inside.
        assert!(state.on_mouse(mev(MouseEventKind::Down(MouseButton::Left), 5, 3)));
        assert!(state.selection_active());
        // The App wraps mouse events; the viewer holds ownership by
        // reporting the selection active flag.
        assert!(!state.scrollbar_dragging());
    }

    #[test]
    fn left_up_ends_selection_drag() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(20, 10);

        state.on_mouse(mev(MouseEventKind::Down(MouseButton::Left), 3, 2));
        state.on_mouse(mev(MouseEventKind::Drag(MouseButton::Left), 10, 4));
        state.on_mouse(mev(MouseEventKind::Up(MouseButton::Left), 10, 4));
        // After Up the selection is committed (still active) but the
        // drag flag is off.
        assert!(state.selection_active());
        assert!(!state.scrollbar_dragging());
    }

    #[test]
    fn left_click_on_close_button_sets_one_shot_request_without_text_area() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.close_button_rect = Some(Rect::new(10, 2, 3, 1));

        assert!(state.on_mouse(mev(MouseEventKind::Down(MouseButton::Left), 11, 2,)));
        assert!(state.take_close_request());
        assert!(!state.take_close_request(), "close request is one-shot");
    }

    #[test]
    fn right_click_without_selection_is_swallowed_silently() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.set_content_metrics_for_test(20, 10);
        // Right-click on Markdown: read-only zone, must return true so
        // App swallows the event (no context-menu popup here either).
        assert!(state.on_mouse(mev(MouseEventKind::Down(MouseButton::Right), 5, 5)));
        assert!(!state.selection_active());
    }

    #[test]
    fn on_mouse_no_op_when_text_area_missing() {
        // Viewer opened but never rendered: text_area is None; every
        // event returns false so the App is free to route elsewhere
        // (there's nothing to interact with yet).
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        assert!(!state.on_mouse(mev(MouseEventKind::Down(MouseButton::Left), 5, 5)));
        assert!(!state.on_mouse(mev(MouseEventKind::ScrollDown, 5, 5)));
    }

    #[test]
    fn extract_columns_handles_wide_chars() {
        // ASCII: straightforward slice.
        assert_eq!(extract_columns("hello world", 0, 4), "hello".to_string());
        assert_eq!(extract_columns("hello world", 6, 10), "world".to_string());
        // Empty range guards.
        assert_eq!(extract_columns("abc", 5, 3), String::new());
        // Wide chars occupy 2 columns each; extraction respects
        // display width.
        assert_eq!(extract_columns("你好abc", 0, 3), "你好".to_string());
        assert_eq!(extract_columns("你好abc", 4, 6), "abc".to_string());
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;
    use std::io::Write;

    /// Deterministic temp file that lives inside the test target dir so
    /// Windows CI never fights with `%TEMP%` permissions.
    fn scratch_file(name: &str, bytes: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("rimeterm-viewer-test-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch dir");
        path.push(name);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(bytes).expect("write");
        path
    }

    #[test]
    fn utf8_markdown_produces_markdown_payload() {
        let path = scratch_file("hello.md", b"# hello\n\nworld");
        match load_markdown_blocking(&path) {
            ViewerPayload::Markdown(text) => assert!(text.contains("# hello")),
            other => panic!("expected Markdown, got {other:?}"),
        }
    }

    #[test]
    fn oversize_markdown_returns_error_payload() {
        // Write just past the cap so the metadata check fires.
        let big: Vec<u8> = vec![b'#'; MARKDOWN_MAX_BYTES as usize + 1];
        let path = scratch_file("big.md", &big);
        match load_markdown_blocking(&path) {
            ViewerPayload::Error(msg) => assert!(msg.contains("MiB Markdown limit")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn non_utf8_markdown_returns_error_payload() {
        // 0xff is invalid UTF-8 in every state.
        let path = scratch_file("bad.md", &[0x66, 0xff, 0x00]);
        match load_markdown_blocking(&path) {
            ViewerPayload::Error(msg) => assert!(msg.contains("UTF-8")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_returns_error_payload() {
        let missing = std::env::temp_dir().join("rimeterm-viewer-missing.md");
        let _ = std::fs::remove_file(&missing);
        match load_markdown_blocking(&missing) {
            ViewerPayload::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn render_overlay_is_noop_when_closed() {
        let mut state = ViewerOverlayState::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        // Prime a distinct glyph so we can prove render_overlay leaves it alone.
        buf.set_string(0, 0, "X", Style::default());
        render_overlay(&mut state, Rect::new(0, 0, 40, 10), &mut buf, None);
        assert_eq!(buf[(0, 0)].symbol(), "X");
    }

    #[test]
    fn render_overlay_draws_border_when_loading() {
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(
            ViewerSource {
                path: PathBuf::from("notes.md"),
                kind: ViewerKind::Markdown,
            },
            None,
        );
        let bounds = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(bounds);
        render_overlay(&mut state, bounds, &mut buf, None);
        // Top-left corner of the block border is `┌`.
        assert_eq!(buf[(0, 0)].symbol(), "┌");
    }

    #[test]
    fn render_into_pane_does_not_draw_in_viewer_close_button() {
        // The viewer no longer paints its own `[×]` — closing is
        // handled by the tab strip's `×` affordance now that viewer
        // instances live as tabs in the files group.
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(
            ViewerSource {
                path: PathBuf::from("notes.md"),
                kind: ViewerKind::Markdown,
            },
            None,
        );
        let bounds = Rect::new(4, 2, 30, 12);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        render_into_pane(&mut state, bounds, &mut buf, None);

        // The three cells that used to host `[×]` must now be part of
        // the plain top border (`─`) or the corner (`┐`).
        let button_x = bounds.x + bounds.width - 4;
        for dx in 0..3 {
            let sym = buf[(button_x + dx, bounds.y)].symbol();
            assert_ne!(sym, "[", "unexpected `[` at dx={dx}");
            assert_ne!(sym, "×", "unexpected `×` at dx={dx}");
            assert_ne!(sym, "]", "unexpected `]` at dx={dx}");
        }
        assert_eq!(state.close_button_rect, None);
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use image::{ImageFormat, RgbaImage};

    fn write_png(name: &str, w: u32, h: u32) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("rimeterm-viewer-img-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch dir");
        path.push(name);
        let img = RgbaImage::from_pixel(w, h, image::Rgba([255, 128, 64, 255]));
        img.save_with_format(&path, ImageFormat::Png).expect("png");
        path
    }

    #[test]
    fn png_produces_image_payload() {
        let path = write_png("solid.png", 4, 4);
        match load_image_blocking(&path) {
            ViewerPayload::Image(ready) => {
                assert_eq!(ready.image.width(), 4);
                assert_eq!(ready.image.height(), 4);
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn missing_image_returns_error_payload() {
        let missing = std::env::temp_dir().join("rimeterm-viewer-missing.png");
        let _ = std::fs::remove_file(&missing);
        match load_image_blocking(&missing) {
            ViewerPayload::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn image_target_size_reset_matches_inner_area() {
        let inner = Rect::new(0, 0, 40, 20);
        let size = image_target_size(inner, 0);
        assert_eq!(size, ratatui::layout::Size::new(40, 20));
    }

    #[test]
    fn image_target_size_zoom_in_grows_and_zoom_out_shrinks() {
        let inner = Rect::new(0, 0, 40, 20);
        let bigger = image_target_size(inner, 3);
        let smaller = image_target_size(inner, -2);
        assert!(bigger.width > 40 && bigger.height > 20);
        assert!(smaller.width < 40 && smaller.height < 20);
    }
}
