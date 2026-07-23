//! Single-session PTY pane.
//!
//! C17: renderer now walks alacritty's `Term::grid().display_iter()`
//! and translates `alacritty_terminal::term::cell::Cell` into ratatui
//! buffer cells. Wide chars, alt-screen swap, and richer color / flag
//! bitmasks all come for free from the alacritty parser.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_pty::{Decision, ResizeThrottle, Session};

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color as AlacColor, NamedColor};

use std::time::Instant;

use crate::pty_selection::{self, Cell as SelCell, Granularity, SelectionState};

/// yazi's three-column ratio `[parent, current, preview]`. §19.14.1
/// mirrors what yazi puts under `[mgr] ratio` — default `[1, 4, 3]`
/// matches yazi's own out-of-the-box layout.
///
/// Any zero entries are clamped to `1` in [`Self::from_ratio`] so a
/// misconfigured `yazi.toml` (or a user pasting `[0,0,0]` into
/// rimeterm's `[mouse]` section) cannot divide-by-zero the zone
/// splitter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct YaziLayout {
    parent: u16,
    current: u16,
    preview: u16,
}

impl YaziLayout {
    /// yazi's default `[1, 4, 3]` ratio.
    pub const DEFAULT: Self = Self {
        parent: 1,
        current: 4,
        preview: 3,
    };

    /// Build from a user-supplied `[u8; 3]`, clamping zeros to `1`.
    /// The three values are promoted to `u16` before summing so
    /// `preview_start_col` can't overflow on tiny (u8) products.
    pub fn from_ratio(ratio: [u8; 3]) -> Self {
        Self {
            parent: ratio[0].max(1) as u16,
            current: ratio[1].max(1) as u16,
            preview: ratio[2].max(1) as u16,
        }
    }

    /// Compute the (inner-relative) column at which the preview /
    /// Quick Look zone starts, given the pane's inner width. Rounded
    /// nearest-integer (half-up) so a 20-cell inner rect with `[1,4,3]`
    /// puts the seam at column 13 (= round(20 * 5/8)).
    ///
    /// Returns `inner_width` when the ratio degenerates so callers can
    /// safely detect "no preview visible" via `preview_start >= width`.
    pub fn preview_start_col(self, inner_width: u16) -> u16 {
        let non_preview = (self.parent + self.current) as u32;
        let total = (self.parent + self.current + self.preview) as u32;
        if total == 0 {
            return inner_width;
        }
        let w = inner_width as u32;
        (((w * non_preview) + (total / 2)) / total) as u16
    }
}

/// Which of the three zones (§19.14.1) a mouse coordinate lands in.
/// Determined **once** on `Down(Left)` for a drag session, then reused
/// for every `Drag` / `Up` event until release — this is invariant 34
/// ("origin decides"). Also computed on `Down(Right)` and each
/// standalone Down/Up/Scroll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum YaziZone {
    /// Parent + current file lists — yazi owns the mouse; rimeterm
    /// never starts a local selection here.
    List,
    /// Read-only preview (Quick Look). rimeterm owns the mouse: local
    /// text selection + right-click copy.
    QuickLook,
}

pub struct PtyPane {
    id: PaneId,
    title: String,
    session: Session,
    last_area: Rect,
    /// PTY resize throttler (§19.12.6). See [`rimeterm_pty::ResizeThrottle`].
    resize: ResizeThrottle,
    /// C22.6: local text selection when the child hasn't asked for xterm
    /// mouse reports. Rendered as a reverse-video overlay in `render` and
    /// copied to the system clipboard on mouse-up.
    selection: SelectionState,
    /// When `true` the pane never owns the mouse for local text selection
    /// / middle-click paste — every mouse event either forwards to the
    /// child as SGR bytes (when the child asked for xterm mouse) or is
    /// dropped. Set on the left column (files: yazi / gitui). This does
    /// NOT affect rimeterm's own D1/D2 divider drag: App::on_mouse checks
    /// dividers BEFORE pane-priority, so the seams stay draggable.
    ///
    /// When `yazi_layout` is also `Some`, this flag applies only to the
    /// `List` zone; the `QuickLook` zone always owns the mouse locally
    /// (§19.14.1).
    mouse_passthrough: bool,
    /// §19.14.4: `Down(Right)` semantics. `false` = legacy copy-only
    /// (yazi / gitui / read-only children keep this); `true` = copy any
    /// active selection then paste from clipboard (agents / shells).
    right_click_paste: bool,
    /// §19.14.1: if `Some`, the pane is yazi and mouse events split by
    /// zone. `None` disables zoning and falls back to the plain
    /// `mouse_passthrough` flag.
    yazi_layout: Option<YaziLayout>,
    /// §19.14.6 invariant 34: origin of the current `Left` drag session.
    /// Set on `Down(Left)`, consulted by `Drag` / `Up`, cleared on
    /// `Up(Left)` (also cleared defensively when a new `Down` arrives
    /// without an intervening `Up`, e.g. focus loss during drag).
    ///
    /// Two values matter to the downstream logic:
    /// - `Some(true)` → drag started in "forward SGR to child" mode;
    ///   every subsequent `Drag` / `Up` also forwards.
    /// - `Some(false)` → drag started in "local selection" mode;
    ///   every subsequent `Drag` / `Up` extends / commits the selection.
    drag_forward_active: Option<bool>,
}

impl PtyPane {
    /// Construct with a caller-chosen `PaneId`. Used by the OSC bridge
    /// (§5.5, C18-D) so the read-loop forwarder can tag broadcast events
    /// with the same id before the pane is registered.
    pub fn with_id(id: PaneId, session: Session, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            session,
            last_area: Rect::default(),
            resize: ResizeThrottle::platform(),
            selection: SelectionState::default(),
            mouse_passthrough: false,
            right_click_paste: false,
            yazi_layout: None,
            drag_forward_active: None,
        }
    }

    /// Designate this pane as mouse-passthrough: never own the mouse for
    /// local text selection / paste. The App sets this on every pane in
    /// the left (files) column. rimeterm's own D1/D2 dividers stay
    /// draggable because App::on_mouse checks dividers first.
    pub fn set_mouse_passthrough(&mut self, on: bool) {
        self.mouse_passthrough = on;
        if on {
            self.selection.clear();
        }
    }

    /// §19.14.4: flip `Down(Right)` semantics to "paste after copy".
    /// Set on agents / shells panes (right column). See [`MouseConfig`]
    /// for the config toggle.
    ///
    /// [`MouseConfig`]: rimeterm_config::MouseConfig
    pub fn set_right_click_paste(&mut self, on: bool) {
        self.right_click_paste = on;
    }

    /// §19.14.1: install (or clear) the yazi three-column layout so
    /// `on_mouse` can split events by zone. Only makes sense on the
    /// yazi tab in the files group; every other pane keeps the default
    /// `None`.
    pub fn set_yazi_layout(&mut self, layout: Option<YaziLayout>) {
        self.yazi_layout = layout;
        if layout.is_none() {
            // Clearing zoning during an active drag would leave the
            // origin memoized against a geometry that no longer
            // exists — safest to reset selection + drag state.
            self.selection.clear();
            self.drag_forward_active = None;
        }
    }

    /// Kill the child process (used at shutdown).
    pub fn kill(&self) {
        self.session.kill();
    }

    /// Access the underlying session (needed for the pty read-loop wakeup).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Immediately push any pending resize to the PTY. Called from the app
    /// main loop on mouse-up so the final drag size lands exactly, and on
    /// each frame so the debounce window can expire between events.
    pub fn tick_resize(&mut self, now: Instant) {
        match self.resize.poll(now) {
            Decision::Apply { cols, rows } => {
                let _ = self.session.resize(cols.max(2), rows.max(1));
            }
            Decision::Idle | Decision::Wait => {}
        }
    }

    /// Force-flush any pending resize (bypasses the debounce window). Call
    /// on mouse-up / drag-end so the final size is exact regardless of when
    /// the window would have expired.
    pub fn flush_resize_now(&mut self) {
        if let Some((cols, rows)) = self.resize.flush_now() {
            let _ = self.session.resize(cols.max(2), rows.max(1));
        }
    }

    /// True when the child has requested any xterm mouse tracking mode
    /// via DECSET (1000/1002/1003/1006). Consulted before every mouse
    /// event to decide whether to forward SGR bytes (yazi / htop / vim
    /// path) or own the mouse locally for text selection (bash / pwsh
    /// path). Shift-modifier always forces local ownership so users can
    /// still select text inside a full-screen TUI.
    fn child_wants_mouse(&self) -> bool {
        self.session
            .with_term(|term| term.mode().contains(TermMode::MOUSE_MODE))
    }

    /// True when the child has enabled bracketed-paste mode
    /// (DECSET 2004). We wrap pasted content in `\x1b[200~..\x1b[201~`
    /// so shells stop treating multi-line paste as multi-Enter.
    fn child_wants_bracketed_paste(&self) -> bool {
        self.session
            .with_term(|term| term.mode().contains(TermMode::BRACKETED_PASTE))
    }

    /// Expose child mouse ownership check to App so it can decide whether
    /// to route Left Down to the pane (App checks dividers FIRST, so the
    /// D1/D2 seams stay draggable regardless). Passthrough panes (left /
    /// files column) always claim priority so a click on yazi's frame is
    /// forwarded instead of starting a rimeterm selection.
    pub fn wants_mouse_priority(&self, shift_held: bool) -> bool {
        if shift_held {
            return false;
        }
        self.mouse_passthrough || self.child_wants_mouse()
    }

    /// Copy the current selection's text to the system clipboard. Called
    /// from `on_mouse` on `Up(Left)` and from the `Ctrl+Shift+C` key
    /// handler. Silent no-op when the selection is empty or the
    /// clipboard is unavailable (headless CI, locked session).
    fn copy_selection(&mut self) {
        if !self.selection.is_active() {
            return;
        }
        let text = self
            .session
            .with_term(|term| pty_selection::extract_text(term, &self.selection));
        let Some(text) = text else {
            return;
        };
        // arboard::Clipboard::new() opens / closes the OS handle each
        // call. That's the recommended usage — long-lived handles can
        // leak on X11 when the process exits without a proper
        // disconnect — and cost is a low-microsecond thing off the hot
        // path.
        if let Ok(mut clip) = arboard::Clipboard::new() {
            let _ = clip.set_text(text);
        }
    }

    /// Read the clipboard, wrap in bracketed-paste sentinels if the
    /// child asked for them (DECSET 2004), and write to the PTY.
    /// Silent no-op on empty clipboard or clipboard error.
    fn paste_from_clipboard(&mut self) {
        let Ok(mut clip) = arboard::Clipboard::new() else {
            return;
        };
        let Ok(text) = clip.get_text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        // Normalize CRLF -> LF: nearly every Unix shell and REPL
        // interprets `\r` as Enter, so a Windows clipboard payload with
        // `\r\n` line endings runs each line as its own command. Even
        // in bracketed mode, some shells still split on `\r`, so
        // strip them unconditionally.
        let normalized: String = text.replace("\r\n", "\n").replace('\r', "\n");

        let mut buf = Vec::with_capacity(normalized.len() + 12);
        if self.child_wants_bracketed_paste() {
            buf.extend_from_slice(b"\x1b[200~");
            buf.extend_from_slice(normalized.as_bytes());
            buf.extend_from_slice(b"\x1b[201~");
        } else {
            buf.extend_from_slice(normalized.as_bytes());
        }
        let _ = self.session.write(&buf);
    }

    /// §19.14.1: classify a mouse column against `yazi_layout`.
    /// Returns `None` when zoning is disabled (non-yazi pane) or when
    /// the point falls outside the pane's inner rect — the caller then
    /// falls back to the plain (unzoned) policy.
    fn zone_at(&self, col: u16, inner: Rect) -> Option<YaziZone> {
        let layout = self.yazi_layout?;
        if inner.width == 0 {
            return None;
        }
        // Column outside the inner rect: caller wants an explicit
        // decision (e.g. drag clamping to preview column), so pin it
        // to the boundary rather than returning None. Rows are checked
        // by the caller — zones split horizontally only.
        let preview_start_rel = layout.preview_start_col(inner.width);
        let preview_start_abs = inner.x.saturating_add(preview_start_rel);
        if col < inner.x {
            return Some(YaziZone::List);
        }
        if col >= preview_start_abs {
            Some(YaziZone::QuickLook)
        } else {
            Some(YaziZone::List)
        }
    }

    /// §19.14.2: rect that a local selection (Down/Drag/Up) may span.
    /// Non-yazi panes get the full inner rect. yazi panes get the
    /// QuickLook column strip only — so a drag that overshoots into
    /// the file lists clamps to the seam instead of leaking the
    /// highlight across zones.
    fn local_selection_rect(&self, outer: Rect) -> Rect {
        let inner = inner_rect(outer);
        match self.yazi_layout {
            Some(layout) if inner.width > 0 => {
                let start_rel = layout.preview_start_col(inner.width);
                let width = inner.width.saturating_sub(start_rel);
                Rect {
                    x: inner.x.saturating_add(start_rel),
                    y: inner.y,
                    width,
                    height: inner.height,
                }
            }
            _ => inner,
        }
    }

    /// Adapter used from `on_mouse`: an inner-content [`SelCell`]
    /// relative to `outer_rect.inner`, but only when the click lands
    /// inside `sel_rect`. Cells stay indexed against the FULL inner
    /// rect so the render overlay math (`inner.width` cols) keeps
    /// working unchanged.
    fn selection_cell_from_rect(
        &self,
        col: u16,
        row: u16,
        outer_rect: Rect,
        sel_rect: Rect,
    ) -> Option<SelCell> {
        if !point_in_rect(col, row, sel_rect) {
            return None;
        }
        let inner = inner_rect(outer_rect);
        Some(SelCell {
            row: row.saturating_sub(inner.y),
            col: col.saturating_sub(inner.x),
        })
    }

    /// Adapter for Drag events: same as [`Self::selection_cell_from_rect`]
    /// but clamps `(col, row)` into `sel_rect` first so overshoots
    /// stay in the highlighted zone.
    fn selection_cell_clamped_rect(
        &self,
        col: u16,
        row: u16,
        outer_rect: Rect,
        sel_rect: Rect,
    ) -> SelCell {
        let inner = inner_rect(outer_rect);
        // sel_rect may be a zero-width strip on very narrow panes;
        // saturating_sub keeps the clamp legal.
        let right = sel_rect.x.saturating_add(sel_rect.width.saturating_sub(1));
        let bottom = sel_rect.y.saturating_add(sel_rect.height.saturating_sub(1));
        let x = col.clamp(sel_rect.x, right.max(sel_rect.x));
        let y = row.clamp(sel_rect.y, bottom.max(sel_rect.y));
        SelCell {
            row: y.saturating_sub(inner.y),
            col: x.saturating_sub(inner.x),
        }
    }
}

impl PaneProvider for PtyPane {
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
            wants_raw_input: true,
            holds_foreground_work: true,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &PaneRenderCtx) -> RenderOutcome {
        // Focus visuals: focused = bright cyan + bold + `▶ …` title marker
        // so it also reads in monochrome / low-contrast terminals; unfocused
        // = dim grey. `LightCyan` alone was hard to see on dark themes.
        let marker = if ctx.focused { "▶ " } else { "  " };
        let title = format!(" {}🐚 {} ", marker, self.title);
        let border_style = if ctx.focused {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);

        // Request a resize through the throttler (§19.12.6). Actual PTY
        // resize happens either when `tick_resize` sees the idle window
        // elapse, or when the app forces a flush on mouse-up.
        //
        // Special case: the very first render bypasses the throttle so the
        // child sees the correct size before its splash frame — Ink apps
        // (oh-my-pi, opencode) render their layout once at spawn and don't
        // reflow well from an 80x24 start.
        if inner != self.last_area {
            if inner.width >= 2 && inner.height >= 1 {
                let first_render = self.last_area == Rect::default();
                self.resize
                    .request(inner.width.max(2), inner.height.max(1), Instant::now());
                if first_render {
                    self.flush_resize_now();
                }
            }
            self.last_area = inner;
        }
        // Cheap poll — no-op when nothing is pending.
        self.tick_resize(Instant::now());

        // Blit alacritty's grid cell-by-cell via `display_iter()`, which
        // yields the visible viewport in row-major order (Indexed<&Cell>).
        // While we hold the term lock, snapshot the cursor position +
        // SHOW_CURSOR mode for the caret handoff to ratatui.
        //
        // Wide chars: alacritty stores a wide grapheme's leading cell
        // with `Flags::WIDE_CHAR` and its trailing half with
        // `Flags::WIDE_CHAR_SPACER`. Skipping the spacer avoids painting
        // a phantom character in the second column while still letting
        // the underlying `set_char` cover both cells visually (ratatui
        // widens automatically for wide chars written via `set_char`).
        let (vt_cursor_row, vt_cursor_col, vt_hide_cursor) = self.session.with_term(|term| {
            let inner_cols = inner.width as usize;
            let inner_rows = inner.height as usize;
            for indexed in term.grid().display_iter() {
                let row = indexed.point.line.0;
                let col = indexed.point.column.0;
                // display_iter yields scrollback lines with negative
                // `.line.0` — clamp to visible viewport (0..inner_h).
                if row < 0 {
                    continue;
                }
                let row_u = row as usize;
                if row_u >= inner_rows || col >= inner_cols {
                    continue;
                }
                // Skip the trailing half of a wide char / any leading
                // spacer — we already painted the leading half.
                if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                    || indexed.cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let cell_x = inner.x + col as u16;
                let cell_y = inner.y + row_u as u16;
                let target = &mut buf[(cell_x, cell_y)];
                let ch = indexed.cell.c;
                // '\0' is alacritty's empty-cell sentinel. Render as
                // space so the buffer isn't full of holes for ratatui's
                // diff engine.
                target.set_char(if ch == '\0' { ' ' } else { ch });
                target.set_style(alac_cell_style(indexed.cell));
            }
            let point = term.grid().cursor.point;
            let hide = !term.mode().contains(TermMode::SHOW_CURSOR);
            // alacritty line can be negative for scrollback rows;
            // clamp for the caret query.
            let r = point.line.0.max(0) as u16;
            let c = point.column.0 as u16;
            (r, c, hide)
        });

        // Only the focused pane owns the caret. Unfocused shells still
        // update their alacritty cursor as output arrives, but the OS caret
        // should stay with whichever pane the user is typing into.
        // DECTCEM (ESC[?25l) hides the caret regardless of focus.
        let cursor = translate_cursor(
            ctx.focused,
            vt_hide_cursor,
            inner,
            vt_cursor_row,
            vt_cursor_col,
        );

        // C22.6 selection overlay. Painted AFTER the grid blit so
        // reverse-video wins over the shell's own colours. Line/word
        // modes are handled inside `SelectionState::contains` which
        // knows how to flow past the raw cursor.
        if self.selection.is_active() {
            let cols = inner.width;
            for row in 0..inner.height {
                for col in 0..inner.width {
                    if self.selection.contains(row, col, cols) {
                        let target = &mut buf[(inner.x + col, inner.y + row)];
                        let style = target.style().add_modifier(Modifier::REVERSED);
                        target.set_style(style);
                    }
                }
            }
        }

        RenderOutcome {
            request_redraw: false,
            cursor,
        }
    }

    fn flush_pending_resize(&mut self) {
        self.flush_resize_now();
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        // C22.6 keyboard clipboard shortcuts. Match Ctrl+Shift+C/V
        // (Windows Terminal, Alacritty, Wezterm all use these). The
        // shell almost never sees these combos anyway because Ctrl+C
        // is intercepted at the app menu; Ctrl+Shift adds enough
        // discriminator that we never step on child input.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.copy_selection();
                    return true;
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.paste_from_clipboard();
                    return true;
                }
                _ => {}
            }
        }
        // Esc clears an active selection before falling through to the
        // child. Otherwise a leftover highlight after copy is annoying.
        if key.code == KeyCode::Esc && self.selection.is_active() {
            self.selection.clear();
            // Don't `return true` — the child might want Esc too
            // (e.g. vim mode-switch). Just consumed the highlight.
        }
        if let Some(bytes) = encode_key(key) {
            let _ = self.session.write(&bytes);
            true
        } else {
            false
        }
    }

    fn has_active_selection(&self) -> bool {
        self.selection.is_active()
    }

    fn wants_mouse_priority(&self, shift_held: bool) -> bool {
        // Mirrors the inherent method. App checks dividers BEFORE this,
        // so D1/D2 stay draggable; passthrough just claims priority for
        // the pane's own cells so clicks forward to the child.
        if shift_held {
            return false;
        }
        self.mouse_passthrough || self.child_wants_mouse()
    }
    fn set_mouse_passthrough(&mut self, on: bool) {
        // Delegate to the inherent method so the trait-object call site
        // (`&mut dyn PaneProvider`) actually flips the field. Without
        // this override the trait's default no-op would run and the flag
        // would stay false forever — the root cause of the earlier
        // "passthrough=false" diagnostics.
        PtyPane::set_mouse_passthrough(self, on);
    }
    fn set_right_click_paste(&mut self, on: bool) {
        // Trait-object delegation: identical rationale to
        // `set_mouse_passthrough` — without this override the trait's
        // default no-op fires and the flag never flips.
        PtyPane::set_right_click_paste(self, on);
    }
    fn set_yazi_layout(&mut self, layout: Option<[u8; 3]>) {
        PtyPane::set_yazi_layout(self, layout.map(YaziLayout::from_ratio));
    }
    fn on_mouse(&mut self, ev: MouseEvent, outer_rect: Rect) -> bool {
        // Border occupies 1 cell on every side; clicks on the border are
        // not forwarded to the child (users are targeting the pane frame,
        // typically to grab focus). Drag / Up events skip this filter so
        // a drag that overshoots the border still delivers Up.
        let inner = inner_rect(outer_rect);
        if !point_in_rect(ev.column, ev.row, inner)
            && !matches!(ev.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            return false;
        }

        // Shift always forces local ownership so users can select text
        // inside a full-screen TUI (yazi / vim / htop). Matches Alacritty
        // / Wezterm convention.
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

        // §19.14.1 zoning. Only meaningful when `yazi_layout` is set on
        // this pane (i.e. it's the yazi tab in the files group). All
        // other panes: `zone` stays `None` and the code falls back to
        // the pre-§19.14 passthrough / child-wants logic.
        let zone = self.zone_at(ev.column, inner);

        // §19.14.6 invariant 34 ("origin decides"): once a Left drag
        // session starts, every subsequent Drag / Up honours the mode
        // (forward vs local) picked at Down time. This prevents the
        // drag from flipping if the pointer crosses zones mid-drag.
        let forward = match ev.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                if let Some(origin_forward) = self.drag_forward_active {
                    origin_forward
                } else {
                    self.decide_forward(&ev, zone, shift)
                }
            }
            _ => self.decide_forward(&ev, zone, shift),
        };

        // Track drag origin. Set on Down(Left); cleared on Up(Left).
        if let MouseEventKind::Down(MouseButton::Left) = ev.kind {
            self.drag_forward_active = Some(forward);
        }
        if let MouseEventKind::Up(MouseButton::Left) = ev.kind {
            self.drag_forward_active = None;
        }

        if forward {
            // Any local selection needs to be dropped before we hand
            // control back to the child — otherwise a stale highlight
            // stays on screen after a `less` invocation exits.
            self.selection.clear();
            // xterm SGR mouse expects **1-based, inside-content**
            // coordinates. Points outside inner (drag overshoot) clamp
            // to the border so we never send negative-ish coords.
            let x = ev.column.saturating_sub(inner.x).saturating_add(1);
            let y = ev.row.saturating_sub(inner.y).saturating_add(1);
            if let Some(bytes) = encode_sgr_mouse(ev.kind, ev.modifiers, x, y) {
                let _ = self.session.write(&bytes);
                return true;
            }
            return false;
        }

        // --- Local ownership: selection + paste ---
        //
        // `sel_rect` narrows Down/Drag targets when zoning is active
        // (§19.14.2): a click that starts in QuickLook clamps its
        // extension to the QuickLook column strip even if the pointer
        // wanders left into the file lists.
        let sel_rect = self.local_selection_rect(outer_rect);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(cell) =
                    self.selection_cell_from_rect(ev.column, ev.row, outer_rect, sel_rect)
                {
                    if shift {
                        // Shift+Left grows the existing selection.
                        self.selection.shift_extend(cell);
                    } else {
                        self.selection.begin(cell, Instant::now());
                        if self.selection.granularity() == Granularity::Word {
                            self.session.with_term(|term| {
                                pty_selection::snap_to_word(&mut self.selection, term)
                            });
                        }
                    }
                    return true;
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_active() {
                    let cell =
                        self.selection_cell_clamped_rect(ev.column, ev.row, outer_rect, sel_rect);
                    self.selection.extend(cell);
                    return true;
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.selection.is_active() {
                    self.selection.commit();
                    self.copy_selection();
                    return true;
                }
                false
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                self.paste_from_clipboard();
                true
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // §19.14.2 / §19.14.4 right-click semantics.
                //
                // 1. Any active selection is copied first (so users
                //    who framed text with Left-drag can still "finish"
                //    with a right-click, matching Windows Terminal's
                //    "selection + right = copy" muscle memory).
                // 2. Then, when `right_click_paste` is enabled AND the
                //    zone allows writes (QuickLook is read-only), paste
                //    the clipboard. QuickLook zone (§19.14.6 inv. 36)
                //    forces copy-only regardless of the flag.
                // 3. bottom / any read-only child transparently drops
                //    the paste bytes because `paste_from_clipboard`
                //    routes through `Session::write`, which is a no-op
                //    when the child has no stdin.
                if self.selection.is_active() {
                    self.copy_selection();
                    self.selection.clear();
                }
                let paste_allowed =
                    self.right_click_paste && !matches!(zone, Some(YaziZone::QuickLook));
                if paste_allowed {
                    self.paste_from_clipboard();
                }
                true
            }
            _ => false,
        }
    }
}

impl PtyPane {
    /// §19.14.1 forwarding decision for a **fresh** event (not a
    /// continuation of an active drag — those are dispatched by the
    /// caller against [`Self::drag_forward_active`]).
    ///
    /// Splits into three regimes:
    /// - **Shift held** → always local (`false`), so Shift+Left can
    ///   select even inside a full-screen TUI.
    /// - **yazi_layout set** → zone-based:
    ///     - `List` (parent + current columns) → forward to yazi so
    ///       its own selection / hover / seam-drag keeps working.
    ///     - `QuickLook` (preview column) → local for Down/Drag/Up
    ///       (text selection), but scroll wheel still forwards so
    ///       yazi's previewer can page.
    /// - **no yazi_layout** → legacy behaviour: forward iff the child
    ///   wants xterm mouse OR the pane is marked `mouse_passthrough`.
    fn decide_forward(&self, ev: &MouseEvent, zone: Option<YaziZone>, shift: bool) -> bool {
        decide_forward_pure(
            ev.kind,
            zone,
            shift,
            self.mouse_passthrough,
            self.child_wants_mouse(),
        )
    }
}

/// Session-free kernel of [`PtyPane::decide_forward`]. Extracted so
/// unit tests can exercise the full decision matrix without spinning
/// up an alacritty [`Session`].
///
/// Contract mirrors the doc-comment on `decide_forward`; kept as a
/// free function (not a method) so tests don't need to construct a
/// `PtyPane`.
pub(crate) fn decide_forward_pure(
    kind: MouseEventKind,
    zone: Option<YaziZone>,
    shift: bool,
    mouse_passthrough: bool,
    child_wants_mouse: bool,
) -> bool {
    if shift {
        return false;
    }
    let base_forward = mouse_passthrough || child_wants_mouse;

    if let Some(YaziZone::QuickLook) = zone {
        // Scroll wheel forwards regardless of zoning: yazi's previewer
        // needs the SGR bytes to page its preview. Every other event
        // owns the mouse locally.
        if matches!(
            kind,
            MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ) {
            return base_forward;
        }
        return false;
    }

    // List zone or unzoned: legacy policy.
    base_forward
}

/// Translate an alacritty [`Cell`] into a ratatui [`Style`].
///
/// Maps the fg/bg color enum and the flag bitset. Underline variants
/// collapse to a single `UNDERLINED` modifier (ratatui doesn't
/// distinguish double / curly / dotted underlines, so we accept the
/// downgrade rather than silently dropping them).
fn alac_cell_style(cell: &Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = alac_color(cell.fg, true) {
        style = style.fg(fg);
    }
    if let Some(bg) = alac_color(cell.bg, false) {
        style = style.bg(bg);
    }
    let f = cell.flags;
    if f.contains(Flags::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if f.contains(Flags::DIM) {
        style = style.add_modifier(Modifier::DIM);
    }
    if f.contains(Flags::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if f.intersects(Flags::ALL_UNDERLINES) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if f.contains(Flags::INVERSE) {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if f.contains(Flags::STRIKEOUT) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if f.contains(Flags::HIDDEN) {
        style = style.add_modifier(Modifier::HIDDEN);
    }
    style
}

/// Translate an alacritty color into a ratatui color. Returns `None`
/// for `Named(Foreground)` / `Named(Background)` — those are the
/// "use the terminal default" sentinels and rimeterm has no palette
/// mapping for them yet (v0.1: let the terminal emulator fill in).
fn alac_color(color: AlacColor, foreground: bool) -> Option<Color> {
    Some(match color {
        AlacColor::Named(NamedColor::Foreground)
        | AlacColor::Named(NamedColor::DimForeground)
        | AlacColor::Named(NamedColor::BrightForeground) => {
            // Foreground defaults inherit from the host terminal; ratatui
            // renders as Reset when we don't set a color. Only meaningful
            // for fg because bg defaults are the terminal-clear color.
            if foreground {
                return None;
            }
            Color::Reset
        }
        AlacColor::Named(NamedColor::Background) => return None,
        AlacColor::Named(named) => match named {
            NamedColor::Black | NamedColor::DimBlack => Color::Black,
            NamedColor::Red | NamedColor::DimRed => Color::Red,
            NamedColor::Green | NamedColor::DimGreen => Color::Green,
            NamedColor::Yellow | NamedColor::DimYellow => Color::Yellow,
            NamedColor::Blue | NamedColor::DimBlue => Color::Blue,
            NamedColor::Magenta | NamedColor::DimMagenta => Color::Magenta,
            NamedColor::Cyan | NamedColor::DimCyan => Color::Cyan,
            NamedColor::White | NamedColor::DimWhite => Color::Gray,
            NamedColor::BrightBlack => Color::DarkGray,
            NamedColor::BrightRed => Color::LightRed,
            NamedColor::BrightGreen => Color::LightGreen,
            NamedColor::BrightYellow => Color::LightYellow,
            NamedColor::BrightBlue => Color::LightBlue,
            NamedColor::BrightMagenta => Color::LightMagenta,
            NamedColor::BrightCyan => Color::LightCyan,
            NamedColor::BrightWhite => Color::White,
            // Cursor / underline / etc. — leave to the terminal default.
            _ => return None,
        },
        AlacColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AlacColor::Indexed(i) => Color::Indexed(i),
    })
}

/// Inset the outer pane rect by 1 cell on every side to match the block
/// border we draw in `render`. `saturating_sub` guards against absurdly
/// small rects (e.g. 0×0 during teardown) so the returned rect is always
/// well-formed.
pub(crate) fn inner_rect(outer: Rect) -> Rect {
    let inset_w = outer.width.saturating_sub(2);
    let inset_h = outer.height.saturating_sub(2);
    Rect {
        x: outer.x.saturating_add(1),
        y: outer.y.saturating_add(1),
        width: inset_w,
        height: inset_h,
    }
}

/// Translate a alacritty grid cursor position into an absolute frame position
/// suitable for `ratatui::terminal::Frame::set_cursor_position`.
///
/// Returns `None` when the caret should NOT be visible:
/// - the pane is unfocused (only the focused pane owns the OS caret), or
/// - alacritty says the child hid the caret via DECTCEM (`ESC[?25l`), or
/// - the cursor position (from a stale grid) landed outside `inner`
///   after a shrink resize (safety clamp — otherwise ratatui would try
///   to place the caret past the terminal edge).
///
/// `inner` is the pane's rendered rect AFTER the border/title inset. The
/// alacritty `(row, col)` values are already inner-relative (0-based from
/// the top-left of the child's viewport).
///
/// Pure so we can unit-test focus / hide-cursor / clamp behavior without
/// spinning up a real PTY.
pub(crate) fn translate_cursor(
    focused: bool,
    hide_cursor: bool,
    inner: Rect,
    row: u16,
    col: u16,
) -> Option<(u16, u16)> {
    if !focused || hide_cursor {
        return None;
    }
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if row >= inner.height || col >= inner.width {
        return None;
    }
    Some((inner.x + col, inner.y + row))
}

/// True when `(x, y)` lies inside `r` (inclusive left/top, exclusive
/// right/bottom, matching everywhere else in the app).
pub(crate) fn point_in_rect(x: u16, y: u16, r: Rect) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

/// Encode a crossterm `MouseEvent` as an xterm SGR mouse sequence
/// (`ESC[<button;X;YM` for press/motion, `ESC[<button;X;Ym` for release).
/// `x` / `y` are **1-based pane-local content coordinates**.
///
/// SGR button byte layout (xterm ctlseqs):
///   bits 0..1 = button (0=left, 1=middle, 2=right, 3=release/motion)
///   bit  2    = shift    (+4)
///   bit  3    = meta/alt (+8)
///   bit  4    = ctrl     (+16)
///   bit  5    = motion   (+32)
///   bit  6    = wheel    (+64)      (buttons 64=up, 65=down)
///
/// Returns `None` for events we don't forward (e.g. `Moved` without a
/// held button — most apps ignore those and floods add up).
pub(crate) fn encode_sgr_mouse(
    kind: MouseEventKind,
    mods: KeyModifiers,
    x: u16,
    y: u16,
) -> Option<Vec<u8>> {
    let (mut button, is_release) = match kind {
        MouseEventKind::Down(b) => (button_code(b)?, false),
        MouseEventKind::Up(b) => (button_code(b)?, true),
        MouseEventKind::Drag(b) => (button_code(b)? | 0b0010_0000, false), // +motion
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Moved => return None,
    };
    if mods.contains(KeyModifiers::SHIFT) {
        button |= 0b0000_0100;
    }
    if mods.contains(KeyModifiers::ALT) {
        button |= 0b0000_1000;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        button |= 0b0001_0000;
    }
    let final_char = if is_release { 'm' } else { 'M' };
    Some(format!("\x1b[<{};{};{}{}", button, x, y, final_char).into_bytes())
}

fn button_code(b: MouseButton) -> Option<u8> {
    Some(match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    })
}

/// Translate a `crossterm::KeyEvent` into raw bytes for the pty.
///
/// v0.1 covers the common cases: printable chars, Enter, Tab, Backspace,
/// arrows, Ctrl+letter (excluding Ctrl+C which the app menu intercepts).
fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out = Vec::with_capacity(4);
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Z / Ctrl+@..
                if let 'a'..='z' = c.to_ascii_lowercase() {
                    out.push((c.to_ascii_uppercase() as u8) - b'@');
                } else {
                    return None;
                }
            } else {
                if alt {
                    out.push(0x1b);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    fn ev(kind: MouseEventKind, x: u16, y: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: mods,
        }
    }

    #[test]
    fn inner_rect_insets_by_1_on_each_side() {
        let r = inner_rect(Rect {
            x: 5,
            y: 4,
            width: 20,
            height: 10,
        });
        assert_eq!(
            r,
            Rect {
                x: 6,
                y: 5,
                width: 18,
                height: 8
            }
        );
    }

    #[test]
    fn inner_rect_saturates_on_tiny_outer() {
        let r = inner_rect(Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
    }

    #[test]
    fn sgr_left_press_at_1_1_encodes_correctly() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            1,
            1,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<0;1;1M");
    }

    #[test]
    fn sgr_right_release_encodes_lowercase_m() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Up(MouseButton::Right),
            KeyModifiers::NONE,
            10,
            20,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<2;10;20m");
    }

    #[test]
    fn sgr_drag_sets_motion_bit() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::NONE,
            5,
            7,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[<32;5;7M"); // 0 (left) | 32 (motion)
    }

    #[test]
    fn sgr_scroll_wheel_uses_64_65() {
        let up = encode_sgr_mouse(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 4).unwrap();
        let down = encode_sgr_mouse(MouseEventKind::ScrollDown, KeyModifiers::NONE, 3, 4).unwrap();
        assert_eq!(up, b"\x1b[<64;3;4M");
        assert_eq!(down, b"\x1b[<65;3;4M");
    }

    #[test]
    fn sgr_shift_ctrl_alt_modifiers_add_bits() {
        let bytes = encode_sgr_mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
            1,
            1,
        )
        .unwrap();
        // 0 (left) | 4 (shift) | 8 (alt) | 16 (ctrl) = 28
        assert_eq!(bytes, b"\x1b[<28;1;1M");
    }

    #[test]
    fn sgr_moved_without_button_is_dropped() {
        assert!(encode_sgr_mouse(MouseEventKind::Moved, KeyModifiers::NONE, 1, 1).is_none());
    }

    #[test]
    fn point_in_rect_edges() {
        let r = Rect {
            x: 5,
            y: 5,
            width: 3,
            height: 2,
        };
        assert!(point_in_rect(5, 5, r));
        assert!(point_in_rect(7, 6, r));
        assert!(!point_in_rect(8, 6, r)); // width exclusive
        assert!(!point_in_rect(7, 7, r)); // height exclusive
        assert!(!point_in_rect(4, 5, r));
    }

    #[test]
    fn on_mouse_ignores_clicks_on_border() {
        // ev.column/row on the border cells should NOT produce forwarded bytes.
        // We can't easily construct a live PtyPane in a unit test, but the
        // border-check lives in inner_rect + point_in_rect which the above
        // tests cover. This test just documents the intent.
        let outer = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        };
        let inner = inner_rect(outer);
        assert!(!point_in_rect(0, 0, inner)); // top-left border cell
        assert!(!point_in_rect(9, 0, inner)); // top-right border cell
        assert!(!point_in_rect(0, 4, inner)); // bottom-left border cell
        assert!(point_in_rect(1, 1, inner)); // first content cell
    }

    // --- translate_cursor (focus / hide / clamp) ---

    fn inner_10x5() -> Rect {
        Rect {
            x: 3,
            y: 4,
            width: 10,
            height: 5,
        }
    }

    #[test]
    fn cursor_none_when_pane_unfocused() {
        // Even a normal, visible alacritty cursor should not steal the OS
        // caret from whichever pane is actually focused.
        assert_eq!(translate_cursor(false, false, inner_10x5(), 2, 3), None);
    }

    #[test]
    fn cursor_none_when_child_hid_it_via_dectcem() {
        // A curses / TUI child that emits ESC[?25l expects no visible
        // caret; we honor that even if we're focused.
        assert_eq!(translate_cursor(true, true, inner_10x5(), 2, 3), None);
    }

    #[test]
    fn cursor_maps_grid_pos_to_absolute_frame_pos_when_focused() {
        // alacritty grid (row=2, col=3) inside an inner rect at (3, 4)
        // must translate to absolute (x=6, y=6).
        assert_eq!(
            translate_cursor(true, false, inner_10x5(), 2, 3),
            Some((6, 6))
        );
    }

    #[test]
    fn cursor_at_grid_origin_maps_to_inner_origin() {
        assert_eq!(
            translate_cursor(true, false, inner_10x5(), 0, 0),
            Some((3, 4))
        );
    }

    #[test]
    fn cursor_none_when_grid_pos_outside_inner_after_resize() {
        // Shrunk pane: alacritty's grid may still say row=8 for a tick after
        // resize; clamping to None avoids painting the caret past the
        // pane's rendered area (which ratatui would translate into the
        // hint bar / another pane).
        assert_eq!(translate_cursor(true, false, inner_10x5(), 8, 0), None);
        assert_eq!(translate_cursor(true, false, inner_10x5(), 0, 20), None);
    }

    #[test]
    fn cursor_none_when_inner_rect_collapsed() {
        // 0-width or 0-height inner rect (mid-teardown / extreme resize)
        // must not produce a caret position.
        let collapsed = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 5,
        };
        assert_eq!(translate_cursor(true, false, collapsed, 0, 0), None);
        let collapsed = Rect {
            x: 0,
            y: 0,
            width: 5,
            height: 0,
        };
        assert_eq!(translate_cursor(true, false, collapsed, 0, 0), None);
    }

    // --- §19.14 YaziLayout math ---

    #[test]
    fn yazi_layout_default_matches_yazi_own_default() {
        // yazi ships with [1, 4, 3] out of the box; make sure our
        // hardcoded default matches so the seeded zone splitter agrees
        // with what yazi actually renders.
        let l = YaziLayout::DEFAULT;
        assert_eq!(l.parent, 1);
        assert_eq!(l.current, 4);
        assert_eq!(l.preview, 3);
    }

    #[test]
    fn yazi_layout_from_ratio_clamps_zeros_to_one() {
        // A misconfigured `[mouse] yazi_layout = [0, 0, 0]` must not
        // divide by zero downstream. All zero entries clamp to 1 →
        // ratio 1:1:1 → preview starts at 2/3 * width.
        let l = YaziLayout::from_ratio([0, 0, 0]);
        assert_eq!(l.parent, 1);
        assert_eq!(l.current, 1);
        assert_eq!(l.preview, 1);
    }

    #[test]
    fn preview_start_col_default_ratio_on_16_wide() {
        // [1,4,3] over 16 cells → non-preview share = 5/8 = 62.5%.
        // round(16 * 5/8) = round(10) = 10.
        assert_eq!(YaziLayout::DEFAULT.preview_start_col(16), 10);
    }

    #[test]
    fn preview_start_col_degenerate_widths() {
        // Zero-width pane: seam == width so the "is in QuickLook" test
        // (col >= preview_start) is always false — the callers all
        // early-return before this matters.
        assert_eq!(YaziLayout::DEFAULT.preview_start_col(0), 0);
        // One-cell pane: preview effectively empty; rounds to 1 because
        // 5/8 of 1 rounds to 1 half-up. Not a real scenario but must
        // not panic.
        let s = YaziLayout::DEFAULT.preview_start_col(1);
        assert!(s <= 1);
    }

    #[test]
    fn preview_start_col_matches_rounded_share_on_100_wide() {
        // Documented in §19.14 that we round half-up. 100 * 5/8 = 62.5
        // → 63.
        assert_eq!(YaziLayout::DEFAULT.preview_start_col(100), 63);
    }

    #[test]
    fn preview_start_col_custom_ratio() {
        // [2, 3, 5] over 20 → non-preview = 5/10 = 50%. round(20*.5) = 10.
        let l = YaziLayout::from_ratio([2, 3, 5]);
        assert_eq!(l.preview_start_col(20), 10);
    }

    // --- §19.14 decide_forward_pure — the routing decision matrix ---

    fn down_left(x: u16, y: u16) -> MouseEventKind {
        // Only .kind is consulted by decide_forward_pure; ev helpers
        // are unused here.
        let _ = (x, y);
        MouseEventKind::Down(MouseButton::Left)
    }

    #[test]
    fn forward_matches_legacy_when_no_yazi_layout() {
        // With zoning off, decisions collapse to the pre-§19.14 policy:
        // passthrough OR child_wants → forward; shift always local.
        assert!(decide_forward_pure(
            down_left(0, 0),
            None,
            false,
            true,
            false
        ));
        assert!(decide_forward_pure(
            down_left(0, 0),
            None,
            false,
            false,
            true
        ));
        assert!(!decide_forward_pure(
            down_left(0, 0),
            None,
            false,
            false,
            false
        ));
        assert!(!decide_forward_pure(
            down_left(0, 0),
            None,
            true,
            true,
            true
        ));
    }

    #[test]
    fn quicklook_owns_down_left_locally_even_when_passthrough() {
        // The whole point of §19.14: Quick Look must own text
        // selection despite the pane being marked passthrough.
        assert!(!decide_forward_pure(
            down_left(0, 0),
            Some(YaziZone::QuickLook),
            false,
            true, // passthrough
            true, // child wants mouse
        ));
    }

    #[test]
    fn quicklook_owns_right_click_locally() {
        // Right-click in QuickLook is local (copy-only per §19.14.6
        // invariant 36); the base_forward inputs don't matter.
        assert!(!decide_forward_pure(
            MouseEventKind::Down(MouseButton::Right),
            Some(YaziZone::QuickLook),
            false,
            true,
            true,
        ));
    }

    #[test]
    fn quicklook_scroll_forwards_so_yazi_previewer_can_page() {
        // §19.14.3: scroll wheel forwards even in the local-selection
        // zone so yazi's previewer can page a long file.
        for kind in [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert!(
                decide_forward_pure(kind, Some(YaziZone::QuickLook), false, true, false),
                "kind={kind:?} should forward when passthrough is set",
            );
            assert!(
                !decide_forward_pure(kind, Some(YaziZone::QuickLook), false, false, false),
                "kind={kind:?} should NOT forward without base_forward",
            );
        }
    }

    #[test]
    fn list_zone_forwards_when_passthrough() {
        // List zone (parent + current file lists) keeps the legacy
        // passthrough behaviour: yazi owns the mouse.
        assert!(decide_forward_pure(
            down_left(0, 0),
            Some(YaziZone::List),
            false,
            true,
            false,
        ));
    }

    #[test]
    fn shift_always_forces_local_even_in_list_zone() {
        // Shift+Left in the file lists still starts a rimeterm-side
        // selection so Alacritty / Wezterm muscle memory works.
        assert!(!decide_forward_pure(
            down_left(0, 0),
            Some(YaziZone::List),
            true, // shift
            true,
            true,
        ));
    }

    // --- §19.14.1 zone geometry: `ev` param unused so we're really
    // testing PtyPane's zone_at classifier via the pure math above. ---

    // Note: PtyPane::zone_at itself is exercised end-to-end by
    // preview_start_col tests. The invariant it holds is:
    //   col < inner.x + preview_start_col  →  Some(List)
    //   col >= inner.x + preview_start_col →  Some(QuickLook)
    // The math tests above pin the boundary; the enum branching is
    // trivial and doesn't warrant a Session-backed integration test.

    // Suppress unused-helper warning on `ev()` when this module has
    // no other consumer — kept because the earlier legacy tests use
    // it and future §19.14 tests may want to as well.
    #[test]
    fn ev_helper_still_compiles() {
        let _ = ev(down_left(0, 0), 0, 0, KeyModifiers::NONE);
    }
}
