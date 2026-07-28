//! Integration coverage for the one-shot pre-native-file-git config
//! migration. See §5.1 of the native-file-git plan.

use rimeterm_config::migrate::{MigrationOutcome, migrate_pre_native_file_git};
use tempfile::tempdir;

/// Missing on-disk config → `NotPresent`. No files created, no error.
#[test]
fn missing_config_returns_not_present() {
    let dir = tempdir().expect("workspace tempdir");
    let path = dir.path().join("config.toml");

    let outcome = migrate_pre_native_file_git(&path).expect("migrate ok");
    assert_eq!(outcome, MigrationOutcome::NotPresent);
    assert!(!path.exists(), "must not create the config file");
    assert!(
        !path
            .with_file_name("config.toml.pre-native-file-git.bak")
            .exists(),
        "must not create a backup"
    );
}

/// Default v0.1 config (yazi + gitui tabs only) is migratable:
/// backup written, new schema landed, second call idempotent.
#[test]
fn default_yazi_gitui_config_migrates_cleanly() {
    let dir = tempdir().expect("workspace tempdir");
    let path = dir.path().join("config.toml");
    let original = r#"
[[files.tabs]]
id = "yazi"
label = "yazi"
command = ["yazi"]
install_hint = "brew install yazi"

[[files.tabs]]
id = "gitui"
label = "gitui"
command = ["gitui"]
"#;
    std::fs::write(&path, original).expect("seed config");

    let outcome = migrate_pre_native_file_git(&path).expect("migrate ok");
    assert_eq!(outcome, MigrationOutcome::Migrated);

    // Backup preserves the ORIGINAL bytes byte-for-byte.
    let backup = path.with_file_name("config.toml.pre-native-file-git.bak");
    assert!(backup.is_file(), "backup must exist");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

    // New schema: `[files].tabs` gone, `[files]` and `[git]` populated.
    let rewritten = std::fs::read_to_string(&path).expect("read new config");
    let parsed: toml::Value = toml::from_str(&rewritten).expect("valid toml");
    let files = parsed
        .get("files")
        .and_then(|v| v.as_table())
        .expect("[files] section");
    assert!(
        !files.contains_key("tabs"),
        "[files].tabs must be erased, got: {rewritten}"
    );
    assert_eq!(files.get("left_dir").and_then(|v| v.as_str()), Some("."));
    assert_eq!(files.get("right_dir").and_then(|v| v.as_str()), Some("."));
    assert_eq!(
        files.get("show_hidden").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(files.get("sort").and_then(|v| v.as_str()), Some("name"));
    assert_eq!(files.get("dual_pane").and_then(|v| v.as_bool()), Some(true));

    let git = parsed
        .get("git")
        .and_then(|v| v.as_table())
        .expect("[git] section");
    assert_eq!(git.get("enabled").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        git.get("commit_limit").and_then(|v| v.as_integer()),
        Some(200)
    );
    assert_eq!(
        git.get("diff_layout").and_then(|v| v.as_str()),
        Some("auto")
    );

    // Second call → already migrated, no work, no clobber of backup.
    let outcome2 = migrate_pre_native_file_git(&path).expect("migrate ok");
    assert_eq!(outcome2, MigrationOutcome::AlreadyMigrated);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), rewritten);
}

/// A user with a custom `[files].tabs` entry (id != yazi/gitui) must not
/// be silently downgraded. The migration returns
/// `CustomEntriesRejected` and leaves the on-disk file unchanged so a
/// caller can prompt.
#[test]
fn custom_files_tabs_entry_rejects_without_overwrite() {
    let dir = tempdir().expect("workspace tempdir");
    let path = dir.path().join("config.toml");
    let original = r#"
[[files.tabs]]
id = "yazi"
label = "yazi"
command = ["yazi"]

[[files.tabs]]
id = "custom"
label = "My Tool"
command = ["custom-tool"]

[[files.tabs]]
id = "another"
label = "Other"
command = ["other"]
"#;
    std::fs::write(&path, original).expect("seed config");

    let outcome = migrate_pre_native_file_git(&path).expect("migrate ok");
    match &outcome {
        MigrationOutcome::CustomEntriesRejected { ids } => {
            assert_eq!(ids, &vec!["custom".to_string(), "another".to_string()]);
        }
        other => panic!("expected CustomEntriesRejected, got {other:?}"),
    }

    // Original file untouched → caller decides what to do next.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    // Backup was still written so operator can inspect / restore later.
    let backup = path.with_file_name("config.toml.pre-native-file-git.bak");
    assert!(backup.is_file(), "backup written even on reject");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
}
