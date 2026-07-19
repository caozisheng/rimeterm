//! Persisted per-workspace layout state.
//!
//! §19.12.9 of the design doc: divider ratios the user has explicitly moved
//! survive rimeterm restarts. Kept **differential** — only diffs from the
//! defaults are stored — but for M4 we just persist the full ratio table
//! keyed by SplitPath. Trivial round-trip; later we can normalize to diffs.
//!
//! Storage location: `${data_dir}/workspaces/${workspace_hash}/layout.state.toml`
//! where `workspace_hash` is a stable hash of the workspace root path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::data_dir;

/// Persisted ratios keyed by split path encoded as a dotted string.
///
/// Empty string = root split; `"0"` = first child of root; `"1.0"` = second
/// child of root, then its first child; etc. Chosen over `Vec<u8>` because it
/// serializes cleanly to TOML.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct LayoutState {
    pub splits: BTreeMap<String, Vec<f32>>,
}

impl LayoutState {
    pub fn is_empty(&self) -> bool {
        self.splits.is_empty()
    }

    /// Encode a `Vec<u8>` split path into the string form used as a map key.
    pub fn encode_path(path: &[u8]) -> String {
        path.iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Decode a map-key string back into a `Vec<u8>` split path.
    pub fn decode_path(s: &str) -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }
        s.split('.')
            .filter_map(|part| part.parse::<u8>().ok())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutStateError {
    #[error("I/O error reading `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error in `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Stable hash of the workspace root path. Uses `std`'s SipHash — good enough
/// for filesystem sharding, we're not signing anything.
pub fn workspace_hash(workspace_root: &Path) -> String {
    use std::hash::{Hash, Hasher};
    // Canonicalize when possible; fall back to lossy string.
    let canon = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canon.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Resolve `${data_dir}/workspaces/${hash}/layout.state.toml`.
pub fn workspace_state_file(workspace_root: &Path) -> Option<PathBuf> {
    let base = data_dir()?;
    Some(
        base.join("workspaces")
            .join(workspace_hash(workspace_root))
            .join("layout.state.toml"),
    )
}

impl LayoutState {
    /// Load from `path`. Missing file → default (not an error).
    pub fn load_or_default(path: &Path) -> Result<Self, LayoutStateError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| LayoutStateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LayoutStateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), LayoutStateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| LayoutStateError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let toml_str = toml::to_string_pretty(self)?;
        std::fs::write(path, toml_str).map_err(|source| LayoutStateError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let cases: &[&[u8]] = &[&[], &[0], &[1, 0], &[0, 3, 5]];
        for &case in cases {
            let s = LayoutState::encode_path(case);
            assert_eq!(LayoutState::decode_path(&s), case);
        }
    }

    #[test]
    fn empty_state_toml_round_trip() {
        let s = LayoutState::default();
        let toml_str = toml::to_string(&s).unwrap();
        let back: LayoutState = toml::from_str(&toml_str).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn state_with_split_round_trips() {
        let mut s = LayoutState::default();
        s.splits.insert("".into(), vec![0.4, 0.6]);
        s.splits.insert("0".into(), vec![0.65, 0.35]);
        let toml_str = toml::to_string(&s).unwrap();
        let back: LayoutState = toml::from_str(&toml_str).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn workspace_hash_is_deterministic() {
        let a = workspace_hash(Path::new("/tmp/foo"));
        let b = workspace_hash(Path::new("/tmp/foo"));
        assert_eq!(a, b);
        let c = workspace_hash(Path::new("/tmp/bar"));
        assert_ne!(a, c);
    }
}
