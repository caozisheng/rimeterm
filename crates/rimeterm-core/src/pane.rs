//! Pane abstraction: the single visual unit in rimeterm.
//!
//! See §2.1 of the design doc. Kernel only knows Pane; concrete providers are
//! implemented by PTY host / WASM host / native panes. This crate defines only
//! the trait shape and the [`PaneId`] namespace; no rendering.

use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::event::KernelEvent;

/// Stable id for a pane instance. Monotonic per-process, never reused.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct PaneId(pub u64);

impl PaneId {
    /// Mint a fresh id. Safe to call from any task.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Capabilities a pane declares up-front, used by focus / permission logic.
#[derive(Copy, Clone, Debug, Default)]
pub struct PaneCaps {
    /// True if the pane wants raw keystrokes while focused (e.g. PTY / editor).
    /// False for command-palette-style panes that use structured input.
    pub wants_raw_input: bool,
    /// True if this pane may hold long-running foreground work (build/test).
    /// Used by the shells group to suppress silent kills.
    pub holds_foreground_work: bool,
}

/// Render context passed to [`PaneProvider::render`].
///
/// Kept minimal on purpose: providers should NOT reach past this into kernel
/// state. Anything they need to observe arrives via [`KernelEvent`].
pub struct PaneRenderCtx<'a> {
    /// Whether this pane currently owns keyboard focus.
    pub focused: bool,
    /// Optional pane-provided title override (else use provider default).
    pub title_override: Option<&'a str>,
}

/// Outcome of a render pass. Providers can request follow-up actions.
#[derive(Copy, Clone, Debug, Default)]
pub struct RenderOutcome {
    /// Ask the runloop to schedule another frame ASAP (streaming output, etc).
    pub request_redraw: bool,

    /// Where the terminal's text caret should sit at the end of the frame,
    /// in ABSOLUTE frame coordinates (not pane-local). `None` = pane doesn't
    /// want a caret (ratatui hides it by default). Only the currently
    /// focused pane's value is honored; the kernel discards requests from
    /// unfocused panes so a shell in the background can't move the caret
    /// out from under the focused one.
    ///
    /// PtyPane producers translate `alacritty grid cursor` into
    /// this rect after adding the pane's inner origin. Providers that
    /// don't own a caret (PlaceholderPane, native menus) leave this
    /// `None`.
    pub cursor: Option<(u16, u16)>,
}

/// Any renderable region in rimeterm is a `PaneProvider`.
///
/// Impl authors MUST NOT:
/// - hold references to other panes,
/// - do blocking I/O in `render` / `on_key` (spawn tokio tasks instead),
/// - assume they are the only pane on screen.
///
/// Impl authors MAY:
/// - subscribe to [`KernelEvent`] via the bus,
/// - request focus via [`PaneRenderCtx`],
/// - allocate scratch buffers, but reuse across frames.
pub trait PaneProvider: Send + 'static {
    /// Stable id assigned by the kernel at construction.
    fn id(&self) -> PaneId;

    /// Human-readable label shown in tab strips / status bars.
    fn title(&self) -> &str;

    /// Advertise capabilities that affect focus / permission logic.
    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }

    /// Rename the pane in place. Return `true` if the new title took effect;
    /// providers that don't support renaming return `false` (default).
    /// Placeholders always accept; PtyPane accepts and re-labels the tab
    /// strip on the next frame.
    fn set_title(&mut self, title: String) -> bool {
        let _ = title;
        false
    }

    /// Draw into `buf` clipped to `area`. Must be pure (no side-effecting I/O).
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &PaneRenderCtx) -> RenderOutcome;

    /// Handle a focused key event. Return `true` if consumed, else the kernel
    /// forwards to global keymap fallbacks.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let _ = key;
        false
    }

    /// Deliver a mouse event that hit inside this pane's outer rect. The
    /// event coordinates are still in **absolute terminal cells**; the
    /// provider is responsible for translating relative to `outer_rect`
    /// and clipping out its own border.
    ///
    /// Return `true` if the event was consumed (e.g. forwarded to a PTY
    /// child as an SGR mouse sequence). Providers with no interest keep
    /// the default no-op and let the app main loop apply its own fallback
    /// (e.g. focus + tab activation on left-click).
    fn on_mouse(&mut self, ev: MouseEvent, outer_rect: Rect) -> bool {
        let _ = (ev, outer_rect);
        false
    }

    /// Deliver a kernel event this pane subscribed to.
    fn on_event(&mut self, ev: &KernelEvent) {
        let _ = ev;
    }

    /// Force any in-flight PTY resize (throttled by §19.12.6) to apply
    /// immediately. NativePane providers keep the default no-op; PtyPane
    /// overrides to flush the pending size to the underlying pseudo-console.
    fn flush_pending_resize(&mut self) {}

    /// When this pane represents a missing external tool and offers a
    /// one-key install shortcut (`[I]`), return the command to run in a
    /// fresh shell tab. `None` means the pane is not installable and
    /// `[I]` should fall through to the normal keymap.
    ///
    /// The command MUST be a single shell line (no embedded newlines);
    /// the App will type it into a new shell tab AS IF the user typed it,
    /// so they can review / edit before hitting Enter. Rimeterm never
    /// auto-executes install commands — the user always confirms with Enter.
    fn install_command(&self) -> Option<&str> {
        None
    }
}
