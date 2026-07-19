//! Focus tracker.
//!
//! v0.1 (M0) had implicit focus (whichever pane wasn't the menu popover). M1
//! introduces multiple panes; someone needs to own "which is focused right
//! now" so the keymap engine can dispatch correctly and providers know whether
//! they should draw a highlighted border.
//!
//! Broadcasts a [`crate::event::KernelEvent::PaneFocused`] every time the
//! focus changes.

use crate::event::{EventBus, KernelEvent};
use crate::pane::PaneId;
use crate::tabs::TabGroupId;

#[derive(Debug)]
pub struct FocusManager {
    focused_pane: Option<PaneId>,
    focused_group: Option<TabGroupId>,
    bus: EventBus,
}

impl FocusManager {
    pub fn new(bus: EventBus) -> Self {
        Self {
            focused_pane: None,
            focused_group: None,
            bus,
        }
    }

    pub fn focused_pane(&self) -> Option<PaneId> {
        self.focused_pane
    }

    /// Alias kept for M0/M1 callers. Prefer [`Self::focused_pane`] in new code.
    pub fn focused(&self) -> Option<PaneId> {
        self.focused_pane
    }

    pub fn focused_group(&self) -> Option<TabGroupId> {
        self.focused_group
    }

    /// Set focus and broadcast the change. No-op when already focused.
    /// The caller passes the tab group the pane belongs to so the app main
    /// loop can direct `Ctrl+T`, `Alt+[/]`, etc. at the right group without
    /// having to walk the tree on every keypress.
    pub fn set_focus(&mut self, pane: PaneId, group: Option<TabGroupId>) {
        let pane_changed = self.focused_pane != Some(pane);
        self.focused_pane = Some(pane);
        self.focused_group = group;
        if pane_changed {
            self.bus.send(KernelEvent::PaneFocused(pane));
        }
    }

    pub fn is_focused(&self, pane: PaneId) -> bool {
        self.focused_pane == Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_focus_updates_pane_and_group() {
        let bus = EventBus::default();
        let mut fm = FocusManager::new(bus);
        let a = PaneId::next();
        let g = crate::tabs::BUILTIN_SHELLS;
        fm.set_focus(a, Some(g));
        assert_eq!(fm.focused_pane(), Some(a));
        assert_eq!(fm.focused_group(), Some(g));
        assert!(fm.is_focused(a));
    }

    #[test]
    fn refocus_same_pane_is_noop() {
        let bus = EventBus::default();
        let mut fm = FocusManager::new(bus);
        let a = PaneId::next();
        fm.set_focus(a, None);
        fm.set_focus(a, None);
        assert_eq!(fm.focused(), Some(a));
    }
}
