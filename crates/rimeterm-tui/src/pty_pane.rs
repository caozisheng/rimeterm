//! Single-session PTY pane.
//!
//! C17: renderer now walks alacritty's `Term::grid().display_iter()`
//! and translates `alacritty_terminal::term::cell::Cell` into ratatui
//! buffer cells. Wide chars, alt-screen swap, and richer color / flag
//! bitmasks all come for free from the alacritty parser.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{
    Block, Borders, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_pty::{Decision, ResizeThrottle, Session};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color as AlacColor, NamedColor};

use std::time::Instant;

use crate::pty_selection::{ClickStreak, Granularity};

pub struct PtyPane {
    id: PaneId,
    title: String,
    session: Session,
    last_area: Rect,
    /// PTY resize throttler (§19.12.6). See [`rimeterm_pty::ResizeThrottle`].
    resize: ResizeThrottle,
    /// C22.6: local text selection when the child hasn't asked for xterm
    /// mouse reports. The selection itself lives inside alacritty's
    /// `Term.selection` in **absolute grid coordinates** (see
    /// [`crate::pty_selection`]), so it rotates with streaming output
    /// and survives viewport scrolling. This field only tracks the UI
    /// state machine: click-streak granularity promotion.
    click_streak: ClickStreak,
    /// Granularity of the current/most recent selection (kept locally
    /// because alacritty's `Selection` doesn't expose its type).
    granularity: Granularity,
    /// Active local text-selection drag: inner rect + last pointer
    /// position. Set on `Down(Left)` in local mode, updated on `Drag`,
    /// cleared on `Up`. Drives App-level sticky routing (Drag/Up keep
    /// reaching this pane after the pointer leaves it) and
    /// `poll_background` edge autoscroll while the pointer rests
    /// above/below the pane.
    text_drag: Option<TextDrag>,
    /// When `true` the pane never owns the mouse for local text selection
    /// / middle-click paste — every mouse event either forwards to the
    /// child as SGR bytes (when the child asked for xterm mouse) or is
    /// dropped. Set on left-column (files) panes. This does NOT affect
    /// rimeterm's own D1/D2 divider drag: App::on_mouse checks dividers
    /// BEFORE pane-priority, so the seams stay draggable.
    mouse_passthrough: bool,
    /// §19.14.4: `Down(Right)` semantics. Two-step protocol: with an
    /// active selection, right-click copies + clears; without one,
    /// `true` pastes the clipboard (agents / shells), `false` swallows
    /// the click (copy-only panes).
    right_click_paste: bool,
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
    /// True only for agent and interactive shell panes.
    scrollback_enabled: bool,
    /// Last rendered local scrollbar hit target. Present only while focused with history.
    scrollbar_rect: Option<Rect>,
    /// Keeps scrollbar ownership through Left drag/up events.
    scrollbar_drag: bool,
    /// P0-2: cached snapshot of the last painted inner region.
    /// Reused as the fast path when nothing that would change the
    /// pane's on-screen output has moved: no alacritty damage, same
    /// area, same focus, same selection state, same scroll offset,
    /// same block-border color. A `Buffer.clone()` blit into the
    /// frame's buffer is O(cells) memcpy vs the `display_iter` walk
    /// with its per-cell style translation.
    ///
    /// `None` on the very first render, after a resize, or after any
    /// tracked-state divergence. Rebuilt to the new inner rect on the
    /// next full paint.
    last_render: Option<Buffer>,
    /// P0-2: scroll offset the last time the grid was painted.
    /// Alacritty's damage tracking flags "user scrolled the viewport"
    /// via `mark_fully_damaged` inside `scroll_display`, but we
    /// double-check here so a race between the read-loop's damage
    /// reset and our render can't leak a stale offset.
    last_display_offset: usize,
    /// P0-2: focus state at the time of the last paint. Focus flips
    /// change the border color and the `▶` title marker — repaint.
    last_focused: bool,
    last_cursor_row: u16,
    last_cursor_col: u16,
    last_hide_cursor: bool,
    last_history_size: usize,
}

/// Live state of a local Left-drag text selection (see
/// [`PtyPane::text_drag`]).
#[derive(Copy, Clone)]
struct TextDrag {
    /// Inner rect captured at `Down` — coordinate frame for `col`/`row`
    /// and the autoscroll edge bands.
    inner: Rect,
    /// Last pointer position (screen coords; may overshoot `inner`).
    col: u16,
    row: u16,
    /// Whether the pointer moved (a `Drag` arrived) or the Down itself
    // was an intentional selection act (Shift+extend, word/line click
    // streak). Distinguishes a commit-worthy drag from a bare click,
    // which must clear instead of copying one cell.
    moved: bool,
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
            click_streak: ClickStreak::default(),
            granularity: Granularity::default(),
            text_drag: None,
            mouse_passthrough: false,
            right_click_paste: false,
            drag_forward_active: None,
            scrollback_enabled: false,
            scrollbar_rect: None,
            scrollbar_drag: false,
            last_render: None,
            last_display_offset: 0,
            last_focused: false,
            last_cursor_row: 0,
            last_cursor_col: 0,
            last_hide_cursor: true,
            last_history_size: 0,
        }
    }

    /// Designate this pane as mouse-passthrough: never own the mouse for
    /// local text selection / paste. The App sets this on every pane in
    /// the left (files) column. rimeterm's own D1/D2 dividers stay
    /// draggable because App::on_mouse checks dividers first.
    pub fn set_mouse_passthrough(&mut self, on: bool) {
        self.mouse_passthrough = on;
        if on {
            self.clear_selection();
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
        let text = self.session.with_term(|term| term.selection_to_string());
        let Some(text) = text else {
            return;
        };
        // crate::clipboard::Clipboard::new() opens / closes the OS handle each
        // call. That's the recommended usage — long-lived handles can
        // leak on X11 when the process exits without a proper
        // disconnect — and cost is a low-microsecond thing off the hot
        // path.
        if let Ok(mut clip) = crate::clipboard::Clipboard::new() {
            let _ = clip.set_text(text);
        }
    }

    /// Read the clipboard, wrap in bracketed-paste sentinels if the
    /// child asked for them (DECSET 2004), and write to the PTY.
    /// Silent no-op on empty clipboard or clipboard error.
    fn paste_from_clipboard(&mut self) {
        let Ok(mut clip) = crate::clipboard::Clipboard::new() else {
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

    /// True when an alacritty-anchored selection exists.
    fn has_selection(&self) -> bool {
        self.session.with_term(|term| term.selection.is_some())
    }

    /// Clear the alacritty-anchored selection and any live drag state.
    fn clear_selection(&mut self) {
        self.text_drag = None;
        if self.has_selection() {
            self.click_streak.reset();
            self.session.with_term_mut(|term| term.selection = None);
            self.session.mark_render_dirty();
        }
    }

    /// Extend the active selection to the last observed drag position
    /// (`text_drag`). Called from the `Drag` handler and from
    /// `poll_background` edge autoscroll. Follows alacritty's
    /// `vi_mode_recompute_selection` protocol (`update` +
    /// `include_all`) so the range is inclusive on both ends regardless
    /// of drag direction.
    fn extend_selection_to_pointer(&mut self) {
        let Some(td) = self.text_drag else { return };
        self.session.with_term_mut(|term| {
            let offset = term.grid().display_offset();
            let point = grid_point_drag(td.col, td.row, td.inner, offset, term);
            selection_extend_to(&mut term.selection, point);
        });
        self.session.mark_render_dirty();
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

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, ctx: &PaneRenderCtx) -> RenderOutcome {
        let buf = frame.buffer_mut();
        // Focus visuals: focused = bright cyan + bold + `▶ …` title marker
        // so it also reads in monochrome / low-contrast terminals; unfocused
        // = dim grey. `LightCyan` alone was hard to see on dark themes.
        //
        // Border painting stays unconditional — it's ~2×(w+h) cells,
        // dwarfed by grid content, and the focus color / marker MUST
        // reflect the current-frame state (P0-2 only skips the grid,
        // not the frame).
        let marker = if ctx.focused { "▶ " } else { "  " };
        let title = format!(" {}🐚 {} ", marker, self.title);
        let border_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
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
        let area_changed = inner != self.last_area;
        if area_changed {
            if inner.width >= 2 && inner.height >= 1 {
                let first_render = self.last_area == Rect::default();
                self.resize
                    .request(inner.width.max(2), inner.height.max(1), Instant::now());
                if first_render {
                    self.flush_resize_now();
                }
            }
            self.last_area = inner;
            // Any stale snapshot points at the OLD abs coords + old
            // dimensions; drop it so the slow path rebuilds.
            self.last_render = None;
        }
        // Cheap poll — no-op when nothing is pending.
        self.tick_resize(Instant::now());

        let session_dirty = self.session.take_render_dirty();
        let refresh_term =
            needs_term_refresh(session_dirty, area_changed, self.last_render.is_some());

        let mut vt_cursor_row = self.last_cursor_row;
        let mut vt_cursor_col = self.last_cursor_col;
        let mut vt_hide_cursor = self.last_hide_cursor;
        let mut history_size = self.last_history_size;
        let mut display_offset = self.last_display_offset;

        if refresh_term {
            (
                vt_cursor_row,
                vt_cursor_col,
                vt_hide_cursor,
                history_size,
                display_offset,
            ) = self.session.with_term_mut(|term| {
                term.reset_damage();
                let display_offset = term.grid().display_offset();
                let history_size = term.total_lines().saturating_sub(term.screen_lines());
                let point = term.grid().cursor.point;
                let hide = !term.mode().contains(TermMode::SHOW_CURSOR) || display_offset > 0;
                let cursor_row = point.line.0.max(0) as u16;
                let cursor_col = point.column.0 as u16;
                let inner_cols = inner.width as usize;
                let inner_rows = inner.height as usize;
                for indexed in term.grid().display_iter() {
                    let Some(row_u) =
                        viewport_row(indexed.point.line.0, display_offset, inner_rows)
                    else {
                        continue;
                    };
                    let col = indexed.point.column.0;
                    if col >= inner_cols {
                        continue;
                    }
                    if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                        || indexed.cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let target = &mut buf[(inner.x + col as u16, inner.y + row_u as u16)];
                    let ch = indexed.cell.c;
                    target.set_char(if ch == '\0' { ' ' } else { ch });
                    target.set_style(alac_cell_style(indexed.cell));
                }
                (cursor_row, cursor_col, hide, history_size, display_offset)
            });

            if inner.width > 0 && inner.height > 0 {
                self.last_render = Some(snapshot_inner(buf, inner));
            }
        } else if let Some(snap) = &self.last_render {
            blit_buffer_at(snap, buf, (inner.x, inner.y));
        }

        // C22.6 selection overlay: alacritty-normalized range in
        // absolute grid coords, mapped back through the current
        // display offset. Painted AFTER the grid blit so reverse-video
        // wins over the shell's own colours. Fetched fresh every frame
        // (cheap uncontended lock) so the overlay tracks drags even on
        // the snapshot fast path.
        let sel_range = self
            .session
            .with_term(|term| term.selection.as_ref().and_then(|s| s.to_range(term)));
        if let Some(sel) = sel_range {
            for row in 0..inner.height {
                let line = Line(i32::from(row) - display_offset as i32);
                for col in 0..inner.width {
                    if sel.contains(Point::new(line, Column(usize::from(col)))) {
                        let target = &mut buf[(inner.x + col, inner.y + row)];
                        let style = target.style().add_modifier(Modifier::REVERSED);
                        target.set_style(style);
                    }
                }
            }
        }

        self.scrollbar_rect = None;
        if self.scrollback_enabled
            && should_show_scrollbar(ctx.focused, history_size)
            && inner.width > 0
            && inner.height > 0
        {
            let scrollbar_rect = Rect {
                x: inner.x.saturating_add(inner.width.saturating_sub(1)),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            // The bar overlays the grid's last column. A wide glyph at
            // the second-to-last column would cover the bar cell and
            // suppress its diff (see `scrollbar` module docs) — narrow
            // it and clear content styling under the lane first.
            crate::scrollbar::prepare_scrollbar_column(buf, scrollbar_rect);
            let position = history_size.saturating_sub(display_offset);
            let mut state = ScrollbarState::new(history_size.saturating_add(inner.height as usize))
                .position(position)
                .viewport_content_length(inner.height as usize);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .render(scrollbar_rect, buf, &mut state);
            self.scrollbar_rect = Some(scrollbar_rect);
        } else {
            self.scrollbar_drag = false;
        }

        // `last_render` always stores the raw grid before selection,
        // scrollbar, and cursor overlays. Those transient layers can change
        // without touching the terminal and are reapplied below every frame.

        // C25.1 cursor: focused pane hands the alacritty cursor
        // position back to App as `RenderOutcome.cursor` so ratatui
        // places the OS caret there (BlinkingBlock — see
        // `TerminalGuard::enter`). Every unfocused pane paints a
        // reverse-video block overlay in-buffer so users still see
        // where each background shell / agent is sitting.
        //
        // Painted AFTER `snapshot_inner` on purpose: the cache holds
        // the raw grid, so when the cursor moves next frame the fast
        // path blits a clean background and we re-apply the overlay
        // at the fresh coordinates. No ghost cursor left behind.
        let cursor_cell = cursor_cell_pos(vt_hide_cursor, inner, vt_cursor_row, vt_cursor_col);
        let cursor = if ctx.focused { cursor_cell } else { None };
        if !ctx.focused
            && let Some((cx, cy)) = cursor_cell
        {
            let target = &mut buf[(cx, cy)];
            let style = target.style().add_modifier(Modifier::REVERSED);
            target.set_style(style);
        }
        self.last_display_offset = display_offset;
        self.last_cursor_row = vt_cursor_row;
        self.last_cursor_col = vt_cursor_col;
        self.last_hide_cursor = vt_hide_cursor;
        self.last_history_size = history_size;
        self.last_focused = ctx.focused;

        RenderOutcome {
            request_redraw: false,
            cursor,
        }
    }

    /// Edge autoscroll while a local text-selection drag rests outside
    /// the pane's vertical bounds. The app calls this every main-loop
    /// iteration (~16 ms idle cadence), so holding the pointer past an
    /// edge scrolls ~60 lines/s without needing mouse movement.
    fn poll_background(&mut self) -> bool {
        let Some(td) = self.text_drag else {
            return false;
        };
        if !self.scrollback_enabled {
            return false;
        }
        let delta = edge_scroll_delta(td.row, td.inner);
        if delta == 0 {
            return false;
        }
        self.session.scroll_lines(delta);
        self.extend_selection_to_pointer();
        true
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
        if key.code == KeyCode::Esc && self.has_selection() {
            self.clear_selection();
            // Don't `return true` — the child might want Esc too
            // (e.g. vim mode-switch). Just consumed the highlight.
        }
        let bytes = encode_key(key);
        if should_snap_to_bottom(self.scrollback_enabled, bytes.as_deref()) {
            self.session.scroll_to_offset(0);
        }
        if let Some(bytes) = bytes {
            let _ = self.session.write(&bytes);
            true
        } else {
            false
        }
    }

    fn has_active_selection(&self) -> bool {
        self.has_selection()
    }

    fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag
    }

    fn text_selection_dragging(&self) -> bool {
        self.text_drag.is_some()
    }

    fn set_scrollback_enabled(&mut self, on: bool) {
        self.scrollback_enabled = on;
        if !on {
            self.scrollbar_rect = None;
            self.scrollbar_drag = false;
        }
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
        if self.scrollback_enabled
            && matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .scrollbar_rect
                .is_some_and(|r| point_in_rect(ev.column, ev.row, r))
        {
            self.scrollbar_drag = true;
            self.drag_forward_active = None;
            self.clear_selection();
            self.scroll_to_scrollbar_row(ev.row);
            return true;
        }
        if matches!(ev.kind, MouseEventKind::Drag(MouseButton::Left)) && self.scrollbar_drag {
            self.scroll_to_scrollbar_row(ev.row);
            return true;
        }
        if matches!(ev.kind, MouseEventKind::Up(MouseButton::Left)) && self.scrollbar_drag {
            self.scrollbar_drag = false;
            return true;
        }

        // Shift always forces local ownership so users can select text
        // inside a full-screen TUI. Matches Alacritty / Wezterm convention.
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

        // §19.14.6 invariant 34 ("origin decides"): once a Left drag
        // session starts, every subsequent Drag / Up honours the mode
        // (forward vs local) picked at Down time.
        let forward = match ev.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                if let Some(origin_forward) = self.drag_forward_active {
                    origin_forward
                } else {
                    self.decide_forward(&ev, shift)
                }
            }
            _ => self.decide_forward(&ev, shift),
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
            self.clear_selection();
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
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if self.scrollback_enabled => {
                // Viewport scrolling only moves the display window;
                // the selection is anchored in absolute grid coords and
                // stays put — no clear needed (alacritty behaviour).
                if let Some(lines) = wheel_scroll_lines(ev.kind) {
                    self.session.scroll_lines(lines);
                    return true;
                }
                false
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !point_in_rect(ev.column, ev.row, inner) {
                    return false;
                }
                let point = self.session.with_term(|term| {
                    grid_point_at(ev.column, ev.row, inner, term.grid().display_offset())
                });
                // Shift+Left on an existing selection extends it toward
                // the clicked cell (xterm convention); the anchor
                // re-anchors at the far end inside the closure below.
                // Without a selection it starts fresh, with
                // streak-promoted granularity.
                let extend = shift && self.has_selection();
                let gran = if extend {
                    self.granularity
                } else {
                    self.click_streak.begin(point, Instant::now())
                };
                self.granularity = gran;
                self.session.with_term_mut(|term| {
                    let anchor = if extend {
                        term.selection
                            .as_ref()
                            .and_then(|s| s.to_range(term))
                            .map(|r| if point <= r.start { r.end } else { r.start })
                    } else {
                        None
                    };
                    selection_begin_or_extend(
                        &mut term.selection,
                        selection_ty(gran),
                        anchor,
                        point,
                    );
                });
                self.text_drag = Some(TextDrag {
                    inner,
                    col: ev.column,
                    row: ev.row,
                    // Word/line streak clicks and Shift+extend are
                    // commits even without movement.
                    moved: extend || gran != Granularity::Char,
                });
                self.session.mark_render_dirty();
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.text_drag.is_none() {
                    return false;
                }
                // Immediate edge autoscroll on movement; holding the
                // pointer still outside an edge is driven by
                // `poll_background`. Same kernel as `poll_background`
                // so the two can't drift.
                if self.scrollback_enabled {
                    let delta = edge_scroll_delta(ev.row, inner);
                    if delta != 0 {
                        self.session.scroll_lines(delta);
                    }
                }
                let mut td = self.text_drag.unwrap();
                td.col = ev.column;
                td.row = ev.row;
                td.moved = true;
                self.text_drag = Some(td);
                self.extend_selection_to_pointer();
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(td) = self.text_drag.take() {
                    if td.moved {
                        // Commit: copy to the clipboard, keep the
                        // highlight (alacritty keeps `Term.selection`
                        // until the next Down / clear).
                        self.copy_selection();
                    } else {
                        // Bare click: drop the one-cell selection the
                        // Down seeded — a click shouldn't clobber the
                        // clipboard with a single char. Inline (not
                        // `clear_selection`) so the click streak stays
                        // primed for the double-click that follows.
                        self.session.with_term_mut(|term| term.selection = None);
                    }
                    self.session.mark_render_dirty();
                    true
                } else {
                    false
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                self.paste_from_clipboard();
                true
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // §19.14.4 right-click semantics (two-step revision):
                // right-click with an active selection COPIES and clears;
                // right-click with no selection PASTES. Splitting the old
                // copy-then-immediately-paste into two clicks lets the
                // user move the caret / click into another pane between
                // copy and paste — they choose where the text lands.
                // Read-only children transparently drop paste bytes
                // because `paste_from_clipboard` routes through
                // `Session::write`, a no-op when the child has no stdin.
                match right_click_action(self.has_selection(), self.right_click_paste) {
                    RightClickAction::CopyThenClear => {
                        self.copy_selection();
                        self.clear_selection();
                    }
                    RightClickAction::Paste => self.paste_from_clipboard(),
                    RightClickAction::None => {}
                }
                true
            }
            _ => false,
        }
    }
}

impl PtyPane {
    /// Forwarding decision for a **fresh** event (not a continuation of
    /// an active drag — those are dispatched by the caller against
    /// [`Self::drag_forward_active`]).
    ///
    /// Splits into two regimes:
    /// - **Shift held** → always local (`false`), so Shift+Left can
    ///   select even inside a full-screen TUI.
    /// - **otherwise** → forward iff the child wants xterm mouse OR the
    ///   pane is marked `mouse_passthrough`.
    fn decide_forward(&self, ev: &MouseEvent, shift: bool) -> bool {
        decide_forward_pure(
            ev.kind,
            shift,
            self.mouse_passthrough,
            self.child_wants_mouse(),
        )
    }

    fn scroll_to_scrollbar_row(&mut self, row: u16) {
        let Some(rect) = self.scrollbar_rect else {
            return;
        };
        let history_size = self.session.scroll_metrics().history_size;
        let offset = scrollbar_offset_for_row(row, rect.y, rect.height, history_size);
        self.session.scroll_to_offset(offset);
    }
}

/// Session-free kernel of [`PtyPane::decide_forward`]. Extracted so unit
/// tests can exercise the full decision matrix without spinning up an
/// alacritty [`Session`].
pub(crate) fn decide_forward_pure(
    _kind: MouseEventKind,
    shift: bool,
    mouse_passthrough: bool,
    child_wants_mouse: bool,
) -> bool {
    if shift {
        return false;
    }
    mouse_passthrough || child_wants_mouse
}
fn needs_term_refresh(session_dirty: bool, area_changed: bool, has_cache: bool) -> bool {
    session_dirty || area_changed || !has_cache
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

fn should_snap_to_bottom(scrollback_enabled: bool, encoded_key: Option<&[u8]>) -> bool {
    scrollback_enabled && encoded_key.is_some()
}

fn wheel_scroll_lines(kind: MouseEventKind) -> Option<i32> {
    match kind {
        MouseEventKind::ScrollUp => Some(3),
        MouseEventKind::ScrollDown => Some(-3),
        _ => None,
    }
}

/// What a `Down(Right)` inside this pane should do, decided by whether
/// text is selected and whether paste-after-copy is enabled:
///
/// - `CopyThenClear` — selection active: copy it to the clipboard and
///   drop the highlight (the "框选 → 右键复制" step).
/// - `Paste` — nothing selected and paste enabled: paste the clipboard
///   at the current cursor (the "右键 → 粘贴" step; the user picks
///   where by positioning the caret first).
/// - `None` — nothing selected and paste disabled (legacy copy-only
///   panes): swallow silently so the click never leaks to the child.
///
/// Splitting the old copy-then-immediately-paste into two right-clicks
/// lets the user choose the paste position between copy and paste.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum RightClickAction {
    CopyThenClear,
    Paste,
    None,
}

fn right_click_action(has_selection: bool, paste_enabled: bool) -> RightClickAction {
    if has_selection {
        RightClickAction::CopyThenClear
    } else if paste_enabled {
        RightClickAction::Paste
    } else {
        RightClickAction::None
    }
}

fn should_show_scrollbar(focused: bool, history_size: usize) -> bool {
    focused && history_size > 0
}
/// Vertical autoscroll delta for a drag pointer resting outside `inner`:
/// +1 line/tick above the top edge, −1 below the bottom, 0 while inside.
/// Shared by the immediate `Drag` handler and the `poll_background`
/// hold-still tick so the two can never drift apart.
fn edge_scroll_delta(row: u16, inner: Rect) -> i32 {
    if inner.height == 0 {
        return 0;
    }
    if row < inner.y {
        1
    } else if row >= inner.y.saturating_add(inner.height) {
        -1
    } else {
        0
    }
}

fn scrollbar_offset_for_row(row: u16, top: u16, height: u16, history_size: usize) -> usize {
    if height <= 1 {
        return history_size;
    }
    let bottom = top.saturating_add(height.saturating_sub(1));
    let row = row.clamp(top, bottom);
    let from_top = usize::from(row.saturating_sub(top));
    let span = usize::from(height - 1);
    history_size.saturating_sub((from_top * history_size + span / 2) / span)
}

/// Screen position → absolute grid point for a click INSIDE `inner`.
/// Inverse of [`viewport_row`]: viewport row `r` maps to grid line
/// `r - display_offset`.
pub(crate) fn grid_point_at(col: u16, row: u16, inner: Rect, display_offset: usize) -> Point {
    Point::new(
        Line(i32::from(row.saturating_sub(inner.y)) - display_offset as i32),
        Column(usize::from(col.saturating_sub(inner.x))),
    )
}

/// Screen position → absolute grid point for a drag that may OVERSHOOT
/// `inner`: horizontally clamped to the content area, vertically
/// extrapolated past the viewport (so edge autoscroll extends the
/// selection into history) then clamped to the grid's absolute bounds.
pub(crate) fn grid_point_drag<D: Dimensions>(
    col: u16,
    row: u16,
    inner: Rect,
    display_offset: usize,
    dims: &D,
) -> Point {
    let right = inner.x.saturating_add(inner.width.saturating_sub(1));
    let column =
        usize::from(col.clamp(inner.x, right).saturating_sub(inner.x)).min(dims.last_column().0);
    let rel = i32::from(row) - i32::from(inner.y);
    let line = (rel - display_offset as i32).clamp(dims.topmost_line().0, dims.bottommost_line().0);
    Point::new(Line(line), Column(column))
}

/// Begin or extend a selection to `point` using alacritty's
/// `vi_mode_recompute_selection` protocol: `update(point, Side::Left)` +
/// `include_all()`. The resulting `to_range` is inclusive on both ends
/// regardless of drag direction, matching every other terminal.
///
/// `anchor` seeds a new selection (fresh Down); pass the far end of the
/// existing range when Shift+Left extends an existing selection.
fn selection_begin_or_extend(
    term_selection: &mut Option<Selection>,
    ty: SelectionType,
    anchor: Option<Point>,
    point: Point,
) {
    let mut sel = Selection::new(ty, anchor.unwrap_or(point), Side::Left);
    sel.update(point, Side::Left);
    sel.include_all();
    *term_selection = Some(sel);
}

/// Extend an in-progress drag: same protocol as
/// [`selection_begin_or_extend`] but keeps the existing selection's
/// type and anchor. Mirrors alacritty's `vi_mode_recompute_selection`
/// filter: a `None` selection is a no-op (drag without Down).
fn selection_extend_to(term_selection: &mut Option<Selection>, point: Point) {
    if let Some(selection) = term_selection.as_mut().filter(|s| !s.is_empty()) {
        selection.update(point, Side::Left);
        selection.include_all();
    }
}
fn selection_ty(g: Granularity) -> SelectionType {
    match g {
        Granularity::Char => SelectionType::Simple,
        Granularity::Word => SelectionType::Semantic,
        Granularity::Line => SelectionType::Lines,
    }
}

fn viewport_row(line: i32, display_offset: usize, viewport_height: usize) -> Option<usize> {
    let row = i64::from(line) + i64::try_from(display_offset).unwrap_or(i64::MAX);
    usize::try_from(row)
        .ok()
        .filter(|row| *row < viewport_height)
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

/// P0-2: capture the current cell contents of `inner` inside `src`
/// into a fresh `Buffer` sized to that rect. The snapshot uses
/// coordinates relative to `inner` (a small `Buffer` at `x=0, y=0`)
/// so it can be blitted back to any equal-sized rect later.
///
/// Cheap: one allocation + `w*h` cell clones. Cells are POD-ish
/// (char + style + short symbol string), no heap allocations per
/// cell for the ASCII / single-BMP case.
pub(crate) fn snapshot_inner(src: &Buffer, inner: Rect) -> Buffer {
    let snap_area = Rect {
        x: 0,
        y: 0,
        width: inner.width,
        height: inner.height,
    };
    let mut snap = Buffer::empty(snap_area);
    for y in 0..inner.height {
        for x in 0..inner.width {
            let src_cell = src[(inner.x + x, inner.y + y)].clone();
            snap[(x, y)] = src_cell;
        }
    }
    snap
}

pub(crate) fn blit_buffer_at(snap: &Buffer, dst: &mut Buffer, dst_origin: (u16, u16)) {
    let (dx, dy) = dst_origin;
    let snap_area = snap.area();
    for y in 0..snap_area.height {
        for x in 0..snap_area.width {
            dst[(dx + x, dy + y)] = snap[(x, y)].clone();
        }
    }
}

/// C25.1: absolute frame coordinates of the cursor cell inside `inner`,
/// or `None` when the cursor should NOT be painted at all.
///
/// The PTY pane paints its cursor entirely in the ratatui buffer — a
/// hollow `▯` glyph in the focused pane, a reverse-video block in
/// every other pane. Callers hand the result to `buf[(x, y)]` and
/// choose the style based on focus.
///
/// Returns `None` when:
/// - the child hid the caret via DECTCEM (`ESC[?25l`), or
/// - the cursor position (from a stale grid) landed outside `inner`
///   after a shrink resize (safety clamp — otherwise we'd write past
///   the pane border).
///
/// `inner` is the pane's rendered rect AFTER the border/title inset.
/// The alacritty `(row, col)` values are already inner-relative
/// (0-based from the top-left of the child's viewport).
///
/// Pure so we can unit-test hide-cursor / clamp behavior without
/// spinning up a real PTY.
pub(crate) fn cursor_cell_pos(
    hide_cursor: bool,
    inner: Rect,
    row: u16,
    col: u16,
) -> Option<(u16, u16)> {
    if hide_cursor {
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
    fn encoded_key_snaps_enabled_scrollback_to_live_bottom() {
        assert!(should_snap_to_bottom(true, Some(b"x")));
        assert!(!should_snap_to_bottom(false, Some(b"x")));
        assert!(!should_snap_to_bottom(true, None));
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

    // --- cursor_cell_pos (C25.1: focus-agnostic cursor cell locator) ---

    fn inner_10x5() -> Rect {
        Rect {
            x: 3,
            y: 4,
            width: 10,
            height: 5,
        }
    }

    #[test]
    fn cursor_cell_maps_grid_pos_to_absolute_frame_pos() {
        // alacritty grid (row=2, col=3) inside an inner rect at (3, 4)
        // must translate to absolute (x=6, y=6).
        assert_eq!(cursor_cell_pos(false, inner_10x5(), 2, 3), Some((6, 6)));
    }

    #[test]
    fn cursor_cell_at_grid_origin_maps_to_inner_origin() {
        assert_eq!(cursor_cell_pos(false, inner_10x5(), 0, 0), Some((3, 4)));
    }

    #[test]
    fn cursor_cell_none_when_child_hid_it_via_dectcem() {
        // DECTCEM ESC[?25l suppresses the cursor entirely; otherwise a
        // curses TUI (vim, less) would sprout a ghost block on its own
        // hidden-cursor line.
        assert_eq!(cursor_cell_pos(true, inner_10x5(), 2, 3), None);
    }

    #[test]
    fn cursor_cell_none_when_grid_pos_outside_inner_after_resize() {
        // Shrunk pane: alacritty's grid may still say row=8 for a tick
        // after resize; clamping to None avoids painting past the pane
        // border (which ratatui would translate into the hint bar /
        // another pane).
        assert_eq!(cursor_cell_pos(false, inner_10x5(), 8, 0), None);
        assert_eq!(cursor_cell_pos(false, inner_10x5(), 0, 20), None);
    }

    #[test]
    fn cursor_cell_none_when_inner_rect_collapsed() {
        // 0-width or 0-height inner rect (mid-teardown / extreme resize)
        // must not produce a paint position.
        let collapsed = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 5,
        };
        assert_eq!(cursor_cell_pos(false, collapsed, 0, 0), None);
        let collapsed = Rect {
            x: 0,
            y: 0,
            width: 5,
            height: 0,
        };
        assert_eq!(cursor_cell_pos(false, collapsed, 0, 0), None);
    }

    #[test]
    fn wheel_scrolls_three_lines_per_notch() {
        assert_eq!(wheel_scroll_lines(MouseEventKind::ScrollUp), Some(3));
        assert_eq!(wheel_scroll_lines(MouseEventKind::ScrollDown), Some(-3));
    }

    #[test]
    fn horizontal_wheel_does_not_move_history() {
        assert_eq!(wheel_scroll_lines(MouseEventKind::ScrollLeft), None);
    }

    #[test]
    fn scrollbar_only_shows_for_focused_pane_with_history() {
        assert!(should_show_scrollbar(true, 1));
        assert!(!should_show_scrollbar(false, 1));
        assert!(!should_show_scrollbar(true, 0));
    }

    #[test]
    fn scrollbar_top_maps_to_oldest_history() {
        assert_eq!(scrollbar_offset_for_row(10, 10, 5, 100), 100);
    }

    #[test]
    fn scrollbar_bottom_maps_to_live_output() {
        assert_eq!(scrollbar_offset_for_row(14, 10, 5, 100), 0);
    }

    #[test]
    fn scrolled_grid_lines_map_into_visible_rows() {
        assert_eq!(viewport_row(-7, 7, 10), Some(0));
        assert_eq!(viewport_row(2, 7, 10), Some(9));
        assert_eq!(viewport_row(3, 7, 10), None);
    }

    // --- decide_forward_pure — the routing decision matrix ---

    fn down_left() -> MouseEventKind {
        MouseEventKind::Down(MouseButton::Left)
    }

    #[test]
    fn forward_when_passthrough_or_child_wants() {
        assert!(decide_forward_pure(down_left(), false, true, false));
        assert!(decide_forward_pure(down_left(), false, false, true));
    }

    #[test]
    fn no_forward_when_neither_passthrough_nor_child_wants() {
        assert!(!decide_forward_pure(down_left(), false, false, false));
    }

    #[test]
    fn shift_always_forces_local_ownership() {
        assert!(!decide_forward_pure(down_left(), true, true, true));
    }

    // --- right_click_action — the two-step Down(Right) protocol ---

    #[test]
    fn right_click_with_selection_always_copies_then_clears() {
        // Selection wins over paste: 框选 → 右键(复制). The highlight is
        // dropped so the following right-click pastes instead of
        // re-copying stale text.
        assert_eq!(
            right_click_action(true, true),
            RightClickAction::CopyThenClear
        );
        assert_eq!(
            right_click_action(true, false),
            RightClickAction::CopyThenClear
        );
    }

    #[test]
    fn right_click_without_selection_pastes_only_when_enabled() {
        assert_eq!(right_click_action(false, true), RightClickAction::Paste);
        assert_eq!(right_click_action(false, false), RightClickAction::None);
    }

    #[test]
    fn ev_helper_still_compiles() {
        let _ = ev(down_left(), 0, 0, KeyModifiers::NONE);
    }
}

#[cfg(test)]
mod p0_2_tests {
    //! P0-2 damage-driven repaint. The full `PtyPane::render` fast/slow
    //! path needs a live `Session` (heavy — spawns a real PTY child),
    //! so we test the pure buffer helpers here. They're the only
    //! rendering-side surface with any nontrivial logic: fetching cells
    //! from `snap` at (0,0)-anchored coords and painting them back at
    //! an arbitrary `(dst_origin)` offset.
    use super::*;

    fn make_src() -> Buffer {
        // A 10×5 buffer with recognisable content at a known origin.
        // The pane's `inner` rect is offset (border eats 1 cell), so
        // reads at `src[(inner.x + dx, inner.y + dy)]` must land on
        // the right cells — this test catches an off-by-origin bug
        // in `snapshot_inner`.
        let mut b = Buffer::empty(Rect::new(0, 0, 10, 5));
        for y in 0..5 {
            for x in 0..10 {
                let ch = ((x + y * 10) % 26 + ('a' as u16)) as u8 as char;
                b[(x, y)].set_char(ch);
            }
        }
        b
    }

    #[test]
    fn idle_cached_pane_does_not_refresh_terminal_state() {
        assert!(!needs_term_refresh(false, false, true));
        assert!(needs_term_refresh(true, false, true));
        assert!(needs_term_refresh(false, true, true));
        assert!(needs_term_refresh(false, false, false));
    }

    #[test]
    fn snapshot_inner_captures_only_the_inset_region() {
        let src = make_src();
        // Simulate a pane whose border makes its inner rect `(1,1)+8×3`.
        let inner = Rect::new(1, 1, 8, 3);
        let snap = snapshot_inner(&src, inner);

        assert_eq!(snap.area().width, 8);
        assert_eq!(snap.area().height, 3);
        // Top-left of snap = the cell at src(1,1), NOT src(0,0). This
        // guards the (0,0)-origin vs `inner.x/y`-origin invariant.
        assert_eq!(snap[(0, 0)].symbol(), src[(1, 1)].symbol());
        assert_eq!(snap[(7, 2)].symbol(), src[(8, 3)].symbol());
    }

    #[test]
    fn blit_buffer_at_paints_at_requested_origin() {
        // A 3×2 snap becomes visible at (5, 2) inside a larger dst.
        let mut snap = Buffer::empty(Rect::new(0, 0, 3, 2));
        snap[(0, 0)].set_char('X');
        snap[(2, 1)].set_char('Y');

        let mut dst = Buffer::empty(Rect::new(0, 0, 20, 10));
        blit_buffer_at(&snap, &mut dst, (5, 2));

        assert_eq!(dst[(5, 2)].symbol(), "X");
        assert_eq!(dst[(7, 3)].symbol(), "Y");
        // Untouched cells outside the target rect stay empty (space).
        assert_eq!(dst[(0, 0)].symbol(), " ");
        assert_eq!(dst[(8, 3)].symbol(), " ");
    }

    #[test]
    fn snapshot_then_blit_roundtrips_content() {
        // End-to-end: capture at one inner rect, paint back at another.
        // This is what P0-2's fast path does when the pane rect stays
        // stable (its typical case; a resize invalidates the cache
        // upstream via `last_render = None`).
        let src = make_src();
        let inner = Rect::new(1, 1, 6, 3);
        let snap = snapshot_inner(&src, inner);

        let mut dst = Buffer::empty(Rect::new(0, 0, 10, 5));
        blit_buffer_at(&snap, &mut dst, (inner.x, inner.y));

        for y in 0..inner.height {
            for x in 0..inner.width {
                assert_eq!(
                    dst[(inner.x + x, inner.y + y)].symbol(),
                    src[(inner.x + x, inner.y + y)].symbol(),
                    "mismatch at ({x},{y})",
                );
            }
        }
    }
}

#[cfg(test)]
mod selection_tests {
    //! Scroll-during-selection regressions. The full `PtyPane::on_mouse`
    //! needs a live `Session` (real PTY child), so these exercise the
    //! same kernels the handlers call: `selection_begin_or_extend`,
    //! `selection_extend_to`, `grid_point_drag`, `edge_scroll_delta`,
    //! and the Term-level rotation/viewport-scroll semantics they rely
    //! on.
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::selection::SelectionRange;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;
    use std::time::Duration;

    /// Static dims with an explicit history so `topmost_line()`
    /// (= -history) is meaningful for the drag-clamp tests.
    #[derive(Copy, Clone, Debug)]
    struct TestDims {
        columns: usize,
        screen_lines: usize,
        history: usize,
    }

    impl Dimensions for TestDims {
        fn total_lines(&self) -> usize {
            self.screen_lines + self.history
        }
        fn screen_lines(&self) -> usize {
            self.screen_lines
        }
        fn columns(&self) -> usize {
            self.columns
        }
    }

    /// 8 cols × 3 rows with "0\r\n1\r\n2\r\n3\r\n4\r\n5" fed in —
    /// 3 visible rows ("3","4","5") + 3 lines of history ("0","1","2"),
    /// mirroring session.rs's `term_with_history`.
    fn term_with_history() -> Term<VoidListener> {
        let dims = TestDims {
            columns: 8,
            screen_lines: 3,
            history: 0,
        };
        let mut term = Term::new(
            TermConfig::default(),
            &dims,
            alacritty_terminal::event::VoidListener,
        );
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"0\r\n1\r\n2\r\n3\r\n4\r\n5");
        term
    }

    fn pt(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    fn range_of(term: &Term<VoidListener>) -> Option<SelectionRange> {
        term.selection.as_ref().and_then(|s| s.to_range(term))
    }

    #[test]
    fn drag_both_directions_inclusive() {
        // Forward drag: anchor (0,0), extend to (2,3).
        let mut term = term_with_history();
        selection_begin_or_extend(
            &mut term.selection,
            SelectionType::Simple,
            Some(pt(0, 0)),
            pt(0, 0),
        );
        selection_extend_to(&mut term.selection, pt(2, 3));
        let r = range_of(&term).expect("range");
        assert_eq!(r.start, pt(0, 0));
        assert_eq!(r.end, pt(2, 3));
        assert!(r.contains(pt(0, 0)));
        assert!(r.contains(pt(2, 3)));
        assert!(r.contains(pt(1, 1)));

        // Backward drag: anchor (2,3), extend up to (0,0) — the
        // original bug was an exclusive end cell (last char missing).
        let mut term = term_with_history();
        selection_begin_or_extend(
            &mut term.selection,
            SelectionType::Simple,
            Some(pt(2, 3)),
            pt(2, 3),
        );
        selection_extend_to(&mut term.selection, pt(0, 0));
        let r = range_of(&term).expect("range");
        assert_eq!(r.start, pt(0, 0));
        assert_eq!(r.end, pt(2, 3));
        assert!(r.contains(pt(2, 3)));
    }

    #[test]
    fn viewport_scroll_keeps_absolute_anchor() {
        // Regression: a selection anchored in *viewport* coords cleared
        // or drifted when the user wheel-scrolled mid-drag. Anchored in
        // absolute grid coords, only the visible window moves.
        let mut term = term_with_history();
        selection_begin_or_extend(
            &mut term.selection,
            SelectionType::Simple,
            Some(pt(0, 0)),
            pt(0, 0),
        );
        selection_extend_to(&mut term.selection, pt(2, 3));
        // Wheel up 2 lines: display_offset 0 → 2. Selection must be
        // untouched in absolute coords.
        term.scroll_display(Scroll::Delta(2));
        let r = range_of(&term).expect("range");
        assert_eq!(r.start, pt(0, 0));
        assert_eq!(r.end, pt(2, 3));
    }

    #[test]
    fn streaming_output_rotates_selection() {
        // Live output while a selection exists scrolls the grid up; the
        // selection must rotate with the content (alacritty does this
        // in scroll_up_relative) instead of pointing at stale lines.
        let mut term = term_with_history();
        // Anchor on history line -1 ("2"), extend to visible line 1.
        selection_begin_or_extend(
            &mut term.selection,
            SelectionType::Simple,
            Some(pt(-1, 0)),
            pt(-1, 0),
        );
        selection_extend_to(&mut term.selection, pt(1, 3));
        // One more output line: every existing line's storage coord
        // drops by 1 ("2" -1 → -2, visible line 1 → 0).
        let mut p: Processor = Processor::new();
        p.advance(&mut term, b"\r\n6");
        let r = range_of(&term).expect("rotate kept selection");
        assert_eq!(r.start, pt(-2, 0));
        assert_eq!(r.end, pt(0, 3));
    }

    #[test]
    fn edge_scroll_delta_directions() {
        let inner = Rect::new(5, 5, 10, 10);
        assert_eq!(edge_scroll_delta(4, inner), 1); // above top
        assert_eq!(edge_scroll_delta(5, inner), 0); // first inner row
        assert_eq!(edge_scroll_delta(14, inner), 0); // last inner row
        assert_eq!(edge_scroll_delta(15, inner), -1); // below bottom
        assert_eq!(edge_scroll_delta(9999, inner), -1);
        assert_eq!(edge_scroll_delta(0, Rect::new(0, 0, 5, 0)), 0); // zero height
    }

    #[test]
    fn grid_point_drag_clamps_to_grid_bounds() {
        // history 3 → topmost_line = -3, bottommost = 2, last col 7.
        let dims = TestDims {
            columns: 8,
            screen_lines: 3,
            history: 3,
        };
        let inner = Rect::new(1, 1, 8, 3);
        // Drag inside the pane at screen row 1 (top of inner), offset 0.
        assert_eq!(grid_point_drag(4, 1, inner, 0, &dims), pt(0, 3));
        // Horizontal overshoot clamps to the last content column.
        assert_eq!(grid_point_drag(200, 1, inner, 0, &dims), pt(0, 7));
        // Vertical overshoot above the viewport with display_offset 2
        // extrapolates past the top and clamps at topmost (-3).
        assert_eq!(grid_point_drag(4, 0, inner, 2, &dims), pt(-3, 3));
        // Vertical overshoot below clamps at bottommost (2).
        assert_eq!(grid_point_drag(4, 99, inner, 2, &dims), pt(2, 3));
    }

    #[test]
    fn bare_click_protocol_keeps_streak_primed() {
        // A bare Down/Up (no Drag) must clear the seeded single-cell
        // selection WITHOUT resetting the click streak — the Up handler
        // inlines the clear instead of calling `clear_selection`.
        let mut term = term_with_history();
        let mut streak = ClickStreak::default();
        let now = Instant::now();
        let g = streak.begin(pt(0, 0), now);
        assert_eq!(g, Granularity::Char);
        selection_begin_or_extend(&mut term.selection, selection_ty(g), None, pt(0, 0));
        assert!(term.selection.is_some());
        // Bare-click Up clears only the selection (inlined in the
        // handler), leaving the streak intact.
        term.selection = None;
        let g2 = streak.begin(pt(0, 0), now + Duration::from_millis(100));
        assert_eq!(g2, Granularity::Word);
    }
}
