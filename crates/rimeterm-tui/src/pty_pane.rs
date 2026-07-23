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
    mouse_passthrough: bool,
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

    /// Convert a mouse column/row (in absolute terminal cells) into an
    /// inner-content selection cell relative to `outer_rect`'s content
    /// area. Returns `None` when the point lies on the border (which
    /// should not start a selection).
    fn selection_cell_from(&self, col: u16, row: u16, outer_rect: Rect) -> Option<SelCell> {
        let inner = inner_rect(outer_rect);
        if !point_in_rect(col, row, inner) {
            return None;
        }
        Some(SelCell {
            row: row - inner.y,
            col: col - inner.x,
        })
    }

    /// Same as [`Self::selection_cell_from`] but clamps the point to
    /// the inner rect so a drag that overshoots the border still
    /// updates the cursor. Used for `Drag` events where the mouse may
    /// have moved beyond the pane while a button is held.
    fn selection_cell_clamped(&self, col: u16, row: u16, outer_rect: Rect) -> SelCell {
        let inner = inner_rect(outer_rect);
        let x = col.clamp(inner.x, inner.x + inner.width.saturating_sub(1));
        let y = row.clamp(inner.y, inner.y + inner.height.saturating_sub(1));
        SelCell {
            row: y - inner.y,
            col: x - inner.x,
        }
    }

    /// Test helper: read the current selection state without cloning
    /// the whole PtyPane. Used by unit tests to assert routing decisions.
    #[cfg(test)]
    pub(crate) fn selection_snapshot(&self) -> SelectionState {
        self.selection.clone()
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
    fn on_mouse(&mut self, ev: MouseEvent, outer_rect: Rect) -> bool {
        // Border occupies 1 cell on every side; clicks on the border are
        // not forwarded to the child (users are targeting the pane frame,
        // typically to grab focus).
        let inner = inner_rect(outer_rect);
        if !point_in_rect(ev.column, ev.row, inner)
            && !matches!(ev.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            return false;
        }

        // C22.6 routing decision. Three cases:
        //
        // 1. Child asked for xterm mouse reports (yazi / htop / vim) AND
        //    user didn't hold Shift → forward SGR bytes to the child.
        // 2. Pane is mouse-passthrough (left / files column) AND user
        //    didn't hold Shift → forward SGR bytes UNCONDITIONALLY, even
        //    if the child hasn't enabled a mouse mode. This is the key
        //    fix: yazi toggles mouse mode on/off depending on its active
        //    view (e.g. its input prompt disables it). If we dropped
        //    events while mouse mode was off, yazi would never receive
        //    the click that re-enters mouse mode, and its internal
        //    divider drag would be dead. Forwarding SGR always lets the
        //    child decide; an app that doesn't understand SGR simply
        //    ignores the bytes.
        // 3. Otherwise (shell prompt, or Shift held) → own the mouse for
        //    local text selection + paste.
        let shift_forces_local = ev.modifiers.contains(KeyModifiers::SHIFT);
        let forward_to_child = !shift_forces_local
            && (self.mouse_passthrough || self.child_wants_mouse());

        if forward_to_child {
            // Any local selection needs to be dropped before we hand
            // control back to the child — otherwise a stale highlight
            // stays on screen after a `less` invocation exits.
            self.selection.clear();
            // xterm SGR mouse expects **1-based, inside-content**
            // coordinates.
            let x = ev.column - inner.x + 1;
            let y = ev.row - inner.y + 1;
            if let Some(bytes) = encode_sgr_mouse(ev.kind, ev.modifiers, x, y) {
                let _ = self.session.write(&bytes);
                return true;
            }
            return false;
        }

        // --- Local ownership: selection + paste ---
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(cell) = self.selection_cell_from(ev.column, ev.row, outer_rect) {
                    if ev.modifiers.contains(KeyModifiers::SHIFT) {
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
                    let cell = self.selection_cell_clamped(ev.column, ev.row, outer_rect);
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
            // Scroll wheel outside a TUI app: mostly cosmetic (Windows
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click on an active selection: copy and clear.
                // This path is only reached when App detected we have a
                // selection and forwarded the event (§C22.6 right-click).
                if self.selection.is_active() {
                    self.copy_selection();
                    self.selection.clear();
                    return true;
                }
                false
            }
            // shells don't scroll their own history through it), let it
            // bubble up so the app-level shortcut key can handle it.
            _ => false,
        }
    }
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
}
