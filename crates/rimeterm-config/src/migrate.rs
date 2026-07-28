//! One-shot migration of pre-native-file-git configs.
//!
//! Rimeterm ≤ v0.1.24 shipped a `[[files.tabs]]` array of external-tool
//! specs (yazi + gitui by default). The native-file-git refactor
//! replaces that with dedicated `[files]` + `[git]` sections and drops
//! the yazi/gitui runtime path entirely. Existing user configs need to
//! be transparently rewritten on first launch of the new build.
//!
//! Behaviour ([`migrate_pre_native_file_git`]):
//!
//! 1. Missing on-disk file → [`MigrationOutcome::NotPresent`].
//! 2. Parses the TOML as [`toml::Value`]. When no `[files].tabs` array
//!    exists → [`MigrationOutcome::AlreadyMigrated`] (no-op, no backup).
//! 3. Otherwise writes a `<path>.pre-native-file-git.bak` sibling with
//!    the ORIGINAL bytes byte-for-byte.
//! 4. Drops any tab whose `id` is `"yazi"` or `"gitui"` silently.
//!    Anything else is a user customisation we can't migrate; their
//!    ids are collected.
//! 5. Erases the `tabs` field. Ensures `[files]` and `[git]` sections
//!    exist and every missing key is filled with the current default.
//! 6. If custom entries remained → returns
//!    [`MigrationOutcome::CustomEntriesRejected`] WITHOUT overwriting
//!    the original file. The backup is kept so an operator can inspect
//!    it. Callers surface the ids to the user.
//! 7. Otherwise rewrites `path` atomically (temp file + rename) and
//!    returns [`MigrationOutcome::Migrated`].

use std::path::Path;

use anyhow::{Context, Result};
use toml::{Value, value::Table};

/// Outcome of a single call to [`migrate_pre_native_file_git`].
#[derive(Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The config file did not exist on disk. Fresh install — nothing
    /// to migrate.
    NotPresent,
    /// The file exists but no `[files].tabs` array was present. Either
    /// a post-migration config or a hand-written new-schema file.
    AlreadyMigrated,
    /// Backup written and config rewritten with the new schema.
    Migrated,
    /// One or more `[files].tabs` entries could not be dropped
    /// automatically (non-yazi/gitui `id`). The backup was written but
    /// the original file was left untouched so the caller can prompt
    /// the user.
    CustomEntriesRejected { ids: Vec<String> },
}

const BACKUP_SUFFIX: &str = "pre-native-file-git.bak";

/// Migrate a `config.toml` at `config_path` in place. See module docs
/// for the full contract.
pub fn migrate_pre_native_file_git(config_path: &Path) -> Result<MigrationOutcome> {
    let original = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MigrationOutcome::NotPresent);
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!("reading config for migration: {}", config_path.display())
            });
        }
    };

    let mut root: Value = toml::from_str(&original)
        .with_context(|| format!("parsing config toml: {}", config_path.display()))?;

    if !has_legacy_files_tabs(&root) {
        return Ok(MigrationOutcome::AlreadyMigrated);
    }

    // Step 3: write backup before we do anything destructive.
    let backup = backup_path(config_path);
    if let Some(parent) = backup.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating backup parent dir: {}", parent.display()))?;
    }
    std::fs::write(&backup, original.as_bytes())
        .with_context(|| format!("writing backup: {}", backup.display()))?;

    // Steps 4 + 5: strip yazi/gitui, collect unmigratable ids, seed
    // new schema fields.
    let rejected = rewrite_root(&mut root);

    if !rejected.is_empty() {
        return Ok(MigrationOutcome::CustomEntriesRejected { ids: rejected });
    }

    // Step 7: atomic write (temp file + rename).
    let serialised = toml::to_string_pretty(&root).context("serialising migrated config")?;
    let tmp = config_path.with_extension("toml.pre-native-file-git.tmp");
    std::fs::write(&tmp, serialised.as_bytes())
        .with_context(|| format!("writing migrated temp file: {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), config_path.display()))?;

    Ok(MigrationOutcome::Migrated)
}

/// True iff `root` has a `[files]` table with a `tabs` array — the
/// hallmark of the pre-native-file-git schema.
fn has_legacy_files_tabs(root: &Value) -> bool {
    root.get("files")
        .and_then(Value::as_table)
        .and_then(|t| t.get("tabs"))
        .is_some_and(Value::is_array)
}

/// Compute the backup sibling for `config_path`. Uses the file name
/// verbatim so `config.toml` → `config.toml.pre-native-file-git.bak`
/// and `foo.tml` → `foo.tml.pre-native-file-git.bak`.
fn backup_path(config_path: &Path) -> std::path::PathBuf {
    let stem = config_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_string());
    let backup_name = format!("{stem}.{BACKUP_SUFFIX}");
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(backup_name),
        _ => std::path::PathBuf::from(backup_name),
    }
}

/// Strip legacy `[files].tabs` from `root`, ensure the new
/// `[files]` + `[git]` defaults exist, and return the list of tab ids
/// we refused to drop (non-yazi/gitui).
fn rewrite_root(root: &mut Value) -> Vec<String> {
    let root_table = root
        .as_table_mut()
        .expect("top-level toml value is a table for any well-formed config");

    let mut rejected: Vec<String> = Vec::new();

    if let Some(Value::Table(files)) = root_table.get_mut("files") {
        if let Some(Value::Array(tabs)) = files.remove("tabs") {
            for entry in tabs {
                let id = entry
                    .as_table()
                    .and_then(|t| t.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match id.as_str() {
                    "yazi" | "gitui" => { /* dropped silently */ }
                    "" => rejected.push("<unnamed>".to_string()),
                    _ => rejected.push(id),
                }
            }
        }
    }

    fill_defaults(root_table, "files", &default_files_table());
    fill_defaults(root_table, "git", &default_git_table());

    rejected
}

/// Ensure `parent[key]` is a table and that every entry of `defaults`
/// present as a key in `defaults` also exists in the on-disk table.
/// Existing keys are left untouched — user customisations survive.
fn fill_defaults(parent: &mut Table, key: &str, defaults: &Table) {
    let entry = parent
        .entry(key.to_string())
        .or_insert_with(|| Value::Table(Table::new()));
    let table = match entry {
        Value::Table(t) => t,
        _ => {
            // Someone put a scalar/array at [files] or [git]. Replace
            // it with the defaults — anything else risks a runtime
            // deserialize crash after we return.
            *entry = Value::Table(Table::new());
            entry
                .as_table_mut()
                .expect("just-installed value is a table")
        }
    };
    for (k, v) in defaults {
        table.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

fn default_files_table() -> Table {
    let mut t = Table::new();
    t.insert("left_dir".into(), Value::String(".".into()));
    t.insert("right_dir".into(), Value::String(".".into()));
    t.insert("show_hidden".into(), Value::Boolean(false));
    t.insert("sort".into(), Value::String("name".into()));
    t.insert("dual_pane".into(), Value::Boolean(true));
    t
}

fn default_git_table() -> Table {
    let mut t = Table::new();
    t.insert("enabled".into(), Value::Boolean(true));
    t.insert("commit_limit".into(), Value::Integer(200));
    t.insert("diff_layout".into(), Value::String("auto".into()));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_path_lands_next_to_config() {
        let p = std::path::Path::new("/etc/rime/config.toml");
        assert_eq!(
            backup_path(p),
            std::path::PathBuf::from("/etc/rime/config.toml.pre-native-file-git.bak")
        );
    }

    #[test]
    fn has_legacy_flag_only_when_tabs_array_present() {
        let with: Value =
            toml::from_str("[[files.tabs]]\nid = \"yazi\"\nlabel=\"y\"\ncommand=[\"y\"]\n")
                .unwrap();
        assert!(has_legacy_files_tabs(&with));

        let without: Value = toml::from_str("[files]\nleft_dir = \".\"\n").unwrap();
        assert!(!has_legacy_files_tabs(&without));

        let empty: Value = toml::from_str("").unwrap();
        assert!(!has_legacy_files_tabs(&empty));
    }
}
