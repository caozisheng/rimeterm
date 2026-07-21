//! C20 Viewer Overlay — snapshot classification and geometry helpers.
//!
//! The overlay is a modal Snapshot: `Alt+V` freezes the last `files:yazi:active`
//! selection and opens a read-only viewer. Yazi keeps its native third-column
//! Quick Look; rimeterm never proxies Yazi's preview widget.
//!
use std::io;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use rimeterm_core::pane::PaneId;

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

/// Payload carried by worker completions. `path` and `generation` both
/// must match the current snapshot for the completion to apply.
#[derive(Debug)]
pub struct ViewerCompletion {
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
    /// Zero-based first visible line of the Markdown snapshot. Ignored
    /// for image sources.
    markdown_scroll: u16,
    /// Ratatui-image scale key (`+ - 0`). Ignored for Markdown.
    image_scale: i16,
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

    /// Open a fresh Loading snapshot for `source`. Bumps the generation,
    /// resets scroll/zoom, and records the pane to restore on close.
    /// Returns the generation the worker MUST echo back — mismatch =
    /// stale completion, drop.
    pub fn open_snapshot(&mut self, source: ViewerSource, return_focus: ReturnFocus) -> Generation {
        self.generation = Generation(self.generation.0.wrapping_add(1));
        self.status = ViewerStatus::Loading;
        self.snapshot = Some(source);
        self.markdown = None;
        self.image = None;
        self.markdown_scroll = 0;
        self.image_scale = 0;
        self.return_focus = return_focus;
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

    /// Adjust Markdown scroll by `delta` lines, clamped to `max_scroll`.
    /// `max_scroll` comes from the rendered layout (Task 5). No-op when
    /// snapshot is not Markdown.
    pub fn scroll_markdown(&mut self, delta: i32, max_scroll: u16) {
        if !matches!(
            self.snapshot.as_ref().map(|s| s.kind),
            Some(ViewerKind::Markdown),
        ) {
            return;
        }
        let current = i32::from(self.markdown_scroll);
        let clamped = current
            .saturating_add(delta)
            .clamp(0, i32::from(max_scroll));
        self.markdown_scroll = clamped as u16;
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

/// Render the modal overlay into `bounds` (already clamped by
/// [`overlay_rect`]). Owns no I/O — parses the stored Markdown source on
/// the render thread, which is bounded to the 8 MiB cap.
///
/// Callers pass `bounds = overlay_rect(workspace).unwrap()` so this
/// function does not need to re-validate the floor.
pub fn render_overlay(
    state: &ViewerOverlayState,
    bounds: Rect,
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL);
    let inner = block.inner(bounds);
    // Clear the modal cells so background PTY glyphs don't bleed through.
    ratatui::widgets::Clear.render(bounds, buf);
    block.render(bounds, buf);
    match (state.status(), state.snapshot().map(|s| s.kind)) {
        (ViewerStatus::Loading, _) => render_message(inner, buf, "Loading…"),
        (ViewerStatus::Error(msg), _) => render_message(inner, buf, msg),
        (ViewerStatus::Ready, Some(ViewerKind::Markdown)) => {
            let source = state.markdown().unwrap_or("");
            let text = tui_markdown::from_str(source);
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((state.markdown_scroll(), 0))
                .render(inner, buf);
        }
        (ViewerStatus::Ready, Some(ViewerKind::Image)) => {
            render_image(state, inner, buf, picker);
        }
        _ => {}
    }
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
        let mut state = ViewerOverlayState::default();
        state.open_snapshot(src("a.md", ViewerKind::Markdown), None);
        state.scroll_markdown(5, 3);
        assert_eq!(state.markdown_scroll(), 3);
        state.scroll_markdown(-100, 3);
        assert_eq!(state.markdown_scroll(), 0);

        state.open_snapshot(src("logo.png", ViewerKind::Image), None);
        state.scroll_markdown(5, 3);
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
        let state = ViewerOverlayState::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        // Prime a distinct glyph so we can prove render_overlay leaves it alone.
        buf.set_string(0, 0, "X", Style::default());
        render_overlay(&state, Rect::new(0, 0, 40, 10), &mut buf, None);
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
        render_overlay(&state, bounds, &mut buf, None);
        // Top-left corner of the block border is `┌`.
        assert_eq!(buf[(0, 0)].symbol(), "┌");
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
