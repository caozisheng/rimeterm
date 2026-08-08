//! `~/.rimeterm/tuxedo/project.txt` — maps `+project` todo.txt tags to
//! workspace root paths.
//!
//! Managed by the Ctrl+J todo → agent dispatch flow: on the first
//! dispatch of a task whose `+project` matches the current workspace
//! name, an entry is written. Subsequent dispatches of the same project
//! from other workspaces route straight to the recorded path.
//!
//! Format: one `<project>=<absolute-path>` per line. Lines starting
//! with `#` are comments; blank lines are ignored. Keys are lowercased
//! for case-insensitive lookup (a `+Work` tag routes to the same entry
//! as `+work`). Values are stored as-written; on load, non-absolute or
//! blank values are dropped with a `warn!`.
//!
//! Reader errors are non-fatal: a bad line skips with a `warn!`
//! (`invalid project.txt line: <raw>`) so one hand-edit typo can't
//! orphan the rest of the file. Missing file → empty map.
//!
//! Writer uses the same write-temp-then-rename pattern as tuxedo's
//! `todo::write_atomic`, so a killed rimeterm or full disk never leaves
//! a half-written `project.txt` on disk.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::paths::tuxedo_dir;

/// In-memory mapping. Keys are lowercase project names (no leading `+`);
/// values are absolute workspace root paths.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectMap {
    entries: BTreeMap<String, PathBuf>,
}

/// Canonical path: `~/.rimeterm/tuxedo/project.txt`.
pub fn project_file() -> Option<PathBuf> {
    tuxedo_dir().map(|d| d.join("project.txt"))
}

impl ProjectMap {
    /// Empty map. Same as `Self::default()`; provided for symmetry with
    /// [`Self::load_or_default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from `~/.rimeterm/tuxedo/project.txt`, or return an empty
    /// map when the file / directory is missing or unreadable. Never
    /// panics; a corrupt file yields a warning + the salvageable
    /// entries.
    pub fn load_or_default() -> Self {
        let Some(path) = project_file() else {
            return Self::default();
        };
        Self::load_from(&path).unwrap_or_default()
    }

    /// Load from an explicit path. Missing file → `Ok(default)`.
    /// I/O errors other than `NotFound` propagate.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        Ok(Self::parse(&body))
    }

    /// Parse an in-memory body. Public so tests can drive it without
    /// touching the filesystem; `load_from` calls it under the hood.
    pub fn parse(body: &str) -> Self {
        let mut entries = BTreeMap::new();
        for (line_no, raw) in body.lines().enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                warn!(line = line_no + 1, raw, "invalid project.txt line: no `=`");
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                warn!(
                    line = line_no + 1,
                    raw, "invalid project.txt line: empty key or value"
                );
                continue;
            }
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                warn!(
                    line = line_no + 1,
                    raw, "invalid project.txt line: value is not an absolute path"
                );
                continue;
            }
            entries.insert(key.to_ascii_lowercase(), path);
        }
        Self { entries }
    }

    /// Serialise to the plain-text on-disk format. Deterministic order
    /// (BTreeMap sorts by key), stable across runs so diffs are
    /// meaningful in dotfile repos.
    pub fn serialise(&self) -> String {
        let mut out = String::new();
        out.push_str("# project.txt — maps +project todo.txt tags to workspace roots.\n");
        out.push_str("# Managed by rimeterm; hand-edits are honoured on next read.\n");
        out.push_str("# One entry per line: `<project>=<absolute-path>`. `#` starts a comment.\n");
        for (key, path) in &self.entries {
            out.push_str(key);
            out.push('=');
            out.push_str(&path.display().to_string());
            out.push('\n');
        }
        out
    }

    /// Persist to `~/.rimeterm/tuxedo/project.txt` atomically. Returns
    /// `Ok(())` even when `project_file()` yields `None` (headless env
    /// with no HOME) — the caller can log the miss but shouldn't block
    /// dispatch on it.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = project_file() else {
            return Ok(());
        };
        self.save_to(&path)
    }

    /// Atomically save to `path` (write `<path>.tmp`, rename). Creates
    /// missing parent directories.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.serialise())?;
        std::fs::rename(&tmp, path)
    }

    /// Lookup by project name. Case-insensitive (`+Work` and `+work`
    /// resolve to the same entry).
    pub fn get(&self, project: &str) -> Option<&Path> {
        self.entries
            .get(&project.to_ascii_lowercase())
            .map(PathBuf::as_path)
    }

    /// Insert or overwrite. `project` is lowercased on the way in so
    /// callers don't have to; the value is stored verbatim.
    pub fn insert(&mut self, project: &str, path: PathBuf) {
        self.entries.insert(project.to_ascii_lowercase(), path);
    }

    /// Iterator over `(lowercased-project, path)` pairs in sorted key
    /// order. Handy for diagnostics.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_path()))
    }

    /// Entry count. Zero for a fresh map or a missing file.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no entries are recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        // Cross-platform absolute path for tests. `PathBuf::from(p)` is
        // absolute on POSIX; on Windows we prefix a drive letter.
        if cfg!(windows) {
            PathBuf::from(format!("C:\\test{}", p.replace('/', "\\")))
        } else {
            PathBuf::from(p)
        }
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        let body = "\n# comment\n  \nrimeterm=/abs/rimeterm\n";
        let map = ProjectMap::parse(body);
        assert_eq!(map.len(), if cfg!(windows) { 0 } else { 1 });
        // On Windows `/abs/rimeterm` is not absolute; the entry drops.
        // That's the point of the test — the OTHER lines (comment,
        // blank) must not confuse the parser.
    }

    #[test]
    fn parse_lowercases_keys() {
        let value = abs("/root");
        let body = format!("Rimeterm={}\n", value.display());
        let map = ProjectMap::parse(&body);
        assert_eq!(map.get("rimeterm"), Some(value.as_path()));
        assert_eq!(map.get("RIMETERM"), Some(value.as_path()));
        assert_eq!(map.get("RiMeTeRm"), Some(value.as_path()));
    }

    #[test]
    fn parse_drops_bad_lines_and_keeps_good() {
        let good = abs("/repo");
        let body = format!(
            "no-equals-here\nempty=\n=no-key\nnot-abs=relative/path\nok={}\n",
            good.display()
        );
        let map = ProjectMap::parse(&body);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("ok"), Some(good.as_path()));
    }

    #[test]
    fn insert_lowercases_key() {
        let mut map = ProjectMap::new();
        map.insert("Foo", abs("/foo"));
        assert_eq!(map.get("foo"), Some(abs("/foo").as_path()));
        assert_eq!(map.get("FOO"), Some(abs("/foo").as_path()));
    }

    #[test]
    fn insert_overwrites_existing_key() {
        let mut map = ProjectMap::new();
        map.insert("bar", abs("/bar1"));
        map.insert("BAR", abs("/bar2"));
        assert_eq!(map.get("bar"), Some(abs("/bar2").as_path()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn serialise_round_trips_through_parse() {
        let mut map = ProjectMap::new();
        map.insert("alpha", abs("/alpha"));
        map.insert("BETA", abs("/beta"));
        let body = map.serialise();
        let parsed = ProjectMap::parse(&body);
        assert_eq!(parsed, map);
    }

    #[test]
    fn serialise_orders_entries_by_key() {
        let mut map = ProjectMap::new();
        map.insert("zebra", abs("/z"));
        map.insert("apple", abs("/a"));
        let body = map.serialise();
        let apple = body.find("apple=").unwrap();
        let zebra = body.find("zebra=").unwrap();
        assert!(apple < zebra, "keys must serialise in sorted order");
    }

    #[test]
    fn save_to_then_load_from_round_trip() {
        let tmp = std::env::temp_dir().join(format!(
            "rimeterm-project-map-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("project.txt");

        let mut map = ProjectMap::new();
        map.insert("rimeterm", abs("/rime"));
        map.insert("other", abs("/other"));
        map.save_to(&path).unwrap();

        let loaded = ProjectMap::load_from(&path).unwrap();
        assert_eq!(loaded, map);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_from_missing_file_yields_default() {
        let phantom =
            std::env::temp_dir().join("rimeterm-project-map-does-not-exist-xyz-123/none.txt");
        let map = ProjectMap::load_from(&phantom).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn save_creates_parent_directory() {
        let tmp = std::env::temp_dir().join(format!(
            "rimeterm-project-map-mkdir-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("nested/deep/project.txt");
        let mut map = ProjectMap::new();
        map.insert("x", abs("/x"));
        map.save_to(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
