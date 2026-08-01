//! Persisted user selection: which zones sit on the map, in what order.
//!
//! Storage is global (per-user), NOT per-workspace — a colleague-in-Berlin
//! doesn't disappear when you `cd` into a different repo. Written atomically
//! via tmp-file-then-rename, mirroring the stock watchlist writer.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ZonesError;

/// One row in the persisted zone list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneEntry {
    /// The raw string the user typed: `"local"`, `"Asia/Shanghai"`,
    /// `"UTC+5:30"`, …
    pub input: String,
    /// Optional user-provided label overriding the default derived from `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ZoneEntry {
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            label: None,
        }
    }
}

/// Persisted list of zones, in display order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZoneList {
    pub entries: Vec<ZoneEntry>,
}

impl ZoneList {
    /// Seed shown on first launch when no `zones.toml` exists yet.
    ///
    /// The `local` entry gets an auto-derived label at render time via
    /// [`crate::handle::ZoneHandle::display_label`], so no location leaks
    /// into the seed data itself.
    pub fn seeded() -> Self {
        Self {
            entries: vec![
                ZoneEntry::new("local"),
                ZoneEntry::new("UTC"),
                ZoneEntry::new("America/New_York"),
                ZoneEntry::new("Europe/London"),
                ZoneEntry::new("Asia/Tokyo"),
            ],
        }
    }

    /// Load from `path`; return the seeded list if the file is missing.
    pub fn load_or_seed(path: &Path) -> Result<Self, ZonesError> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::seeded()),
            Err(e) => Err(e.into()),
        }
    }

    /// Save atomically: write to `{path}.tmp`, then rename over `path`.
    pub fn save(&self, path: &Path) -> Result<(), ZonesError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Append a zone by input string if not already present (dedup on
    /// exact `input` match). Returns `true` if the entry was added.
    pub fn push_unique(&mut self, input: impl Into<String>) -> bool {
        let input = input.into();
        if self.entries.iter().any(|e| e.input == input) {
            return false;
        }
        self.entries.push(ZoneEntry::new(input));
        true
    }

    /// Remove the entry at `index`; returns the removed entry (if any).
    pub fn remove(&mut self, index: usize) -> Option<ZoneEntry> {
        if index >= self.entries.len() {
            return None;
        }
        Some(self.entries.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_list_includes_local_and_utc() {
        let list = ZoneList::seeded();
        assert!(list.entries.iter().any(|e| e.input == "local"));
        assert!(list.entries.iter().any(|e| e.input == "UTC"));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut list = ZoneList::seeded();
        list.entries[0].label = Some("Home".into());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        list.save(&path).unwrap();
        let loaded = ZoneList::load_or_seed(&path).unwrap();
        assert_eq!(loaded, list);
    }

    #[test]
    fn missing_file_returns_seeded_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zones.toml");
        let loaded = ZoneList::load_or_seed(&path).unwrap();
        assert_eq!(loaded, ZoneList::seeded());
    }

    #[test]
    fn push_unique_dedups_by_exact_input() {
        let mut list = ZoneList::default();
        assert!(list.push_unique("Asia/Shanghai"));
        assert!(!list.push_unique("Asia/Shanghai"));
        assert!(list.push_unique("Asia/Tokyo"));
        assert_eq!(list.entries.len(), 2);
    }

    #[test]
    fn remove_out_of_bounds_is_noop() {
        let mut list = ZoneList::seeded();
        let n = list.entries.len();
        assert!(list.remove(99).is_none());
        assert_eq!(list.entries.len(), n);
    }
}
