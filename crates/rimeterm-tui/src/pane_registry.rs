//! Store for pane providers, keyed by [`PaneId`].
//!
//! Kept as an owned `HashMap<PaneId, Box<dyn PaneProvider>>` rather than
//! interior mutability so the caller (the app main loop) can borrow one pane
//! mutably while immutably observing the layout tree.

use std::collections::HashMap;

use rimeterm_core::pane::{PaneId, PaneProvider};

pub struct PaneRegistry {
    map: HashMap<PaneId, Box<dyn PaneProvider>>,
}

impl PaneRegistry {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, pane: Box<dyn PaneProvider>) -> PaneId {
        let id = pane.id();
        self.map.insert(id, pane);
        id
    }

    pub fn remove(&mut self, id: PaneId) -> Option<Box<dyn PaneProvider>> {
        self.map.remove(&id)
    }

    pub fn get(&self, id: PaneId) -> Option<&dyn PaneProvider> {
        self.map.get(&id).map(|b| b.as_ref())
    }

    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut dyn PaneProvider> {
        self.map.get_mut(&id).map(|b| b.as_mut())
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.map.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for PaneRegistry {
    fn default() -> Self {
        Self::new()
    }
}
