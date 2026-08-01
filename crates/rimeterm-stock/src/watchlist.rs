use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Market;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchEntry {
    pub market: Market,
    pub symbol: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchList {
    pub entries: Vec<WatchEntry>,
}

impl WatchList {
    #[must_use]
    pub fn seeded() -> Self {
        Self {
            entries: vec![
                WatchEntry {
                    market: Market::AShare,
                    symbol: "600519".to_string(),
                    name: "贵州茅台".to_string(),
                },
                WatchEntry {
                    market: Market::HongKong,
                    symbol: "00700".to_string(),
                    name: "腾讯控股".to_string(),
                },
                WatchEntry {
                    market: Market::Us,
                    symbol: "AAPL".to_string(),
                    name: "Apple Inc.".to_string(),
                },
            ],
        }
    }

    pub fn load_or_seed(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).map_err(|source| Error::ParseWatchlist {
                path: path.to_path_buf(),
                source,
            }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Self::seeded()),
            Err(source) => Err(Error::ReadWatchlist {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| write_error(path, source))?;
        let encoded = toml::to_string_pretty(self)?;
        let (temp_path, mut temp) = create_temp_file(path)?;

        let persist_result = (|| -> io::Result<()> {
            temp.write_all(encoded.as_bytes())?;
            temp.flush()?;
            temp.sync_all()?;
            drop(temp);
            replace_file(&temp_path, path)
        })();

        if let Err(source) = persist_result {
            let _cleanup = fs::remove_file(&temp_path);
            return Err(write_error(path, source));
        }
        Ok(())
    }

    pub fn add(&mut self, mut entry: WatchEntry) -> bool {
        let Ok(normalized) = entry.market.normalize_symbol(&entry.symbol) else {
            return false;
        };
        entry.symbol = normalized;
        if self
            .entries
            .iter()
            .any(|current| current.market == entry.market && current.symbol == entry.symbol)
        {
            false
        } else {
            self.entries.push(entry);
            true
        }
    }

    pub fn remove(&mut self, market: Market, symbol: &str) -> bool {
        let normalized = market
            .normalize_symbol(symbol)
            .unwrap_or_else(|_| symbol.trim().to_ascii_uppercase());
        let old_len = self.entries.len();
        self.entries
            .retain(|entry| entry.market != market || entry.symbol != normalized);
        self.entries.len() != old_len
    }

    pub fn entries_for(&self, market: Market) -> impl Iterator<Item = &WatchEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.market == market)
    }
}

fn create_temp_file(path: &Path) -> Result<(PathBuf, fs::File)> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("watchlist.toml");
    let process_id = std::process::id();
    for attempt in 0_u16..=u16::MAX {
        let temp_path = parent.join(format!(".{stem}.{process_id}.{attempt}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(write_error(path, source)),
        }
    }
    Err(write_error(
        path,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no unique temporary name available",
        ),
    ))
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temp_path, path)
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    match fs::rename(temp_path, path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let backup =
                path.with_extension(format!("rimeterm-stock-backup-{}", std::process::id()));
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(path, &backup)?;
            match fs::rename(temp_path, path) {
                Ok(()) => {
                    fs::remove_file(backup)?;
                    Ok(())
                }
                Err(rename_error) => {
                    let _restore = fs::rename(&backup, path);
                    Err(rename_error)
                }
            }
        }
        Err(source) => Err(source),
    }
}

fn write_error(path: &Path, source: io::Error) -> Error {
    Error::WriteWatchlist {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::{WatchEntry, WatchList};
    use crate::Market;

    #[test]
    fn add_rejects_duplicate_market_and_normalized_symbol() {
        let mut list = WatchList {
            entries: Vec::new(),
        };
        let entry = WatchEntry {
            market: Market::HongKong,
            symbol: "700".to_string(),
            name: "腾讯控股".to_string(),
        };
        assert!(list.add(entry.clone()));
        assert!(!list.add(entry));
        assert_eq!(list.entries[0].symbol, "00700");
    }

    #[test]
    fn remove_matches_normalized_symbol() {
        let mut list = WatchList::seeded();
        assert!(list.remove(Market::HongKong, "700.hk"));
        assert!(!list.remove(Market::HongKong, "700"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watchlist.toml");
        let expected = WatchList::seeded();
        expected.save_atomic(&path).unwrap();
        assert_eq!(WatchList::load_or_seed(&path).unwrap(), expected);
    }

    #[test]
    fn missing_watchlist_returns_seeded_entries() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            WatchList::load_or_seed(directory.path().join("missing.toml")).unwrap(),
            WatchList::seeded()
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let input = "entries = []\nextra = true\n";
        assert!(toml::from_str::<WatchList>(input).is_err());
    }
}
