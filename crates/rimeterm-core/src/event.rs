//! Kernel event bus.
//!
//! §2.5 of the design doc: the only inter-plugin channel. Broadcast semantics
//! with bounded capacity; a slow subscriber falls behind gracefully and reports
//! lag via [`tokio::sync::broadcast::error::RecvError::Lagged`].

use std::path::PathBuf;

use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::pane::PaneId;

/// Everything the kernel broadcasts. Plugins subscribe or ignore per event.
#[derive(Clone, Debug)]
pub enum KernelEvent {
    /// yazi (in files:yazi:active) reported a directory change. Contract A:
    /// consumers MAY log / update status / feed agents, but MUST NOT auto-cd
    /// any shell — see §19.3-A.
    YaziCwdChanged { origin: PaneId, path: PathBuf },

    /// A shell tab reported its `$PWD` via OSC 7. Broadcast, no side effects.
    ShellCwdChanged { origin: PaneId, tab_id: String, path: PathBuf },

    /// yazi cursor landed on a file (§19.3-B trigger source).
    FileSelected { origin: PaneId, path: PathBuf },

    /// Configuration file was reloaded from disk; consumers reread their slice.
    ConfigReloaded,

    /// A pane became focused; useful for status bar and agent context refresh.
    PaneFocused(PaneId),

    /// Request the kernel to close and quit. Emitted by menu / Ctrl+Q.
    QuitRequested,
}

/// Thin wrapper over `broadcast::Sender` so call sites don't touch tokio types
/// directly. Clone freely; each clone shares the same underlying channel.
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<KernelEvent>,
}

impl EventBus {
    /// Create a bus with the given capacity. 1024 is the default in §3.2 —
    /// callers should stick to that unless they have a reason.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event. Returns the number of live receivers; ignore for
    /// fire-and-forget. Failure means every subscriber is gone.
    pub fn send(&self, ev: KernelEvent) -> usize {
        self.tx.send(ev).unwrap_or(0)
    }

    /// New subscriber stream. Consumers should treat `Err(Lagged)` as a hint
    /// to catch up (e.g. drop stale state and re-request).
    pub fn subscribe(&self) -> BroadcastStream<KernelEvent> {
        BroadcastStream::new(self.tx.subscribe())
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}
