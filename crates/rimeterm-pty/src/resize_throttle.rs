//! PTY resize throttler (§19.12.6 of the design doc).
//!
//! Problem: dragging a divider at 60 Hz means [`crate::session::Session::resize`]
//! is called 60 times per second. On Windows the underlying
//! `ResizePseudoConsole` costs ~200 μs per call, and old shells (`bash < 5`,
//! older `fish`) may fully redraw on every SIGWINCH. That produces visible
//! jitter during drag.
//!
//! Solution: coalesce successive size requests to at most one PTY resize per
//! `idle_window` cycle. During a rapid drag the throttler updates a pending
//! target on every request; only after the caller sits idle for the window
//! does it emit a [`Decision::Apply`]. A mouse-up (drag end) can force an
//! immediate final flush via [`ResizeThrottle::flush_now`].
//!
//! Constants match the design doc:
//! - Unix / macOS: 80 ms
//! - Windows (ConPTY): 120 ms
//!
//! Kept intentionally pure (no `tokio::time`, no `Instant::now()` inside the
//! decision path) so unit tests can drive `now` deterministically.

use std::time::{Duration, Instant};

/// Debounce window per platform (§19.12.6). ConPTY resize is markedly more
/// expensive than `TIOCSWINSZ` on Unix, so we leave a bigger idle gap.
#[cfg(windows)]
pub const PLATFORM_RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);

#[cfg(not(windows))]
pub const PLATFORM_RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);

/// What the caller should do right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// Nothing pending — no PTY resize needed.
    Idle,
    /// A target is pending but the idle window hasn't elapsed yet. The caller
    /// SHOULD NOT resize the PTY yet; keep polling.
    Wait,
    /// Time to send the target size to the PTY.
    Apply { cols: u16, rows: u16 },
}

/// Pure state machine — the caller supplies `now` on every tick.
#[derive(Debug, Clone)]
pub struct ResizeThrottle {
    /// Debounce window; different platforms pick different defaults but the
    /// caller may override per-instance for testing.
    pub idle_window: Duration,
    /// Last size we actually sent to the PTY.
    last_applied: Option<(u16, u16)>,
    /// Requested size + timestamp of the last request. `None` means no work.
    pending: Option<Pending>,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    cols: u16,
    rows: u16,
    /// When the size was last changed (rolls forward on every fresh request).
    last_change: Instant,
}

impl ResizeThrottle {
    pub fn new(idle_window: Duration) -> Self {
        Self {
            idle_window,
            last_applied: None,
            pending: None,
        }
    }

    /// Convenience constructor that picks the platform default.
    pub fn platform() -> Self {
        Self::new(PLATFORM_RESIZE_DEBOUNCE)
    }

    /// Report that the desired size is now `(cols, rows)`. If it matches the
    /// last-applied size, this is a no-op. Otherwise the throttler bumps its
    /// `last_change` timestamp so [`Self::poll`] waits another window.
    pub fn request(&mut self, cols: u16, rows: u16, now: Instant) {
        if self.last_applied == Some((cols, rows)) {
            self.pending = None;
            return;
        }
        // If we already had a pending change to the same size, keep the
        // *original* timestamp so a stable size flushes as soon as the window
        // expires. Only bump on genuine size changes.
        if let Some(p) = self.pending {
            if p.cols == cols && p.rows == rows {
                return;
            }
        }
        self.pending = Some(Pending {
            cols,
            rows,
            last_change: now,
        });
    }

    /// Ask the throttler for its next action at time `now`.
    ///
    /// The caller performs the physical PTY resize when this returns
    /// [`Decision::Apply`]; the throttler records the applied size and clears
    /// the pending slot atomically.
    pub fn poll(&mut self, now: Instant) -> Decision {
        let Some(p) = self.pending else {
            return Decision::Idle;
        };
        if now.saturating_duration_since(p.last_change) < self.idle_window {
            return Decision::Wait;
        }
        self.last_applied = Some((p.cols, p.rows));
        self.pending = None;
        Decision::Apply {
            cols: p.cols,
            rows: p.rows,
        }
    }

    /// Force an immediate flush of any pending size (used on mouse-up so the
    /// final drag size lands exactly). Returns the size to apply, if any.
    pub fn flush_now(&mut self) -> Option<(u16, u16)> {
        let p = self.pending.take()?;
        self.last_applied = Some((p.cols, p.rows));
        Some((p.cols, p.rows))
    }

    /// Whether a pending request is waiting for the window to elapse.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Testing helper — inspect the last size actually sent to the PTY.
    #[cfg(test)]
    pub fn last_applied(&self) -> Option<(u16, u16)> {
        self.last_applied
    }
}

impl Default for ResizeThrottle {
    fn default() -> Self {
        Self::platform()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn no_request_is_idle() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        assert_eq!(r.poll(t0()), Decision::Idle);
    }

    #[test]
    fn single_request_waits_then_applies() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        let t = t0();
        r.request(120, 34, t);
        assert_eq!(r.poll(t + Duration::from_millis(50)), Decision::Wait);
        assert_eq!(
            r.poll(t + Duration::from_millis(100)),
            Decision::Apply { cols: 120, rows: 34 }
        );
        // After apply, nothing pending.
        assert_eq!(r.poll(t + Duration::from_millis(200)), Decision::Idle);
        assert_eq!(r.last_applied(), Some((120, 34)));
    }

    #[test]
    fn identical_size_is_a_noop() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        let t = t0();
        r.request(120, 34, t);
        r.poll(t + Duration::from_millis(100)); // apply
        r.request(120, 34, t + Duration::from_millis(200));
        assert!(!r.has_pending());
    }

    #[test]
    fn further_change_resets_window() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        let t = t0();
        r.request(80, 24, t);
        r.request(120, 34, t + Duration::from_millis(50));
        // Only 30 ms after the second request — still Wait.
        assert_eq!(r.poll(t + Duration::from_millis(80)), Decision::Wait);
        // 130 ms after the second request — flushes to the *newest* size.
        assert_eq!(
            r.poll(t + Duration::from_millis(150)),
            Decision::Apply { cols: 120, rows: 34 }
        );
    }

    #[test]
    fn flush_now_bypasses_window() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        let t = t0();
        r.request(120, 34, t);
        assert_eq!(r.flush_now(), Some((120, 34)));
        assert!(!r.has_pending());
        // A poll right after still returns Idle since we recorded last_applied.
        assert_eq!(r.poll(t + Duration::from_millis(5)), Decision::Idle);
    }

    #[test]
    fn same_target_before_apply_keeps_original_timestamp() {
        let mut r = ResizeThrottle::new(Duration::from_millis(80));
        let t = t0();
        r.request(120, 34, t);
        // Repeating the exact same target should not push the window forward.
        r.request(120, 34, t + Duration::from_millis(60));
        assert_eq!(
            r.poll(t + Duration::from_millis(85)),
            Decision::Apply { cols: 120, rows: 34 }
        );
    }
}
