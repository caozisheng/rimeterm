use rimeterm_config::files_state::{FileSideState, FilesState};
use tempfile::tempdir;

#[test]
fn invalid_saved_directories_fall_back_to_workspace_root() {
    let root = tempdir().expect("workspace tempdir");
    let missing = root.path().join("missing");
    let state = FilesState {
        active: FileSideState::Right,
        left_dir: missing.clone(),
        right_dir: missing,
        show_hidden: true,
        sort: "name".to_owned(),
        dual_pane: true,
    };

    let resolved = state.resolve_for_workspace(root.path());

    assert_eq!(resolved.left_dir, root.path());
    assert_eq!(resolved.right_dir, root.path());
    assert_eq!(resolved.active, FileSideState::Right);
}

#[test]
fn save_and_load_round_trip_is_atomic_and_lossless() {
    let root = tempdir().expect("workspace tempdir");
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).expect("create nested");
    let path = root.path().join("files.state.toml");
    let state = FilesState {
        active: FileSideState::Left,
        left_dir: root.path().to_path_buf(),
        right_dir: nested,
        show_hidden: true,
        sort: "extension".to_owned(),
        dual_pane: false,
    };

    state.save_to(&path).expect("save state");
    let loaded = FilesState::load_or_default(&path).expect("load state");

    assert_eq!(loaded, state);
    assert!(!path.with_extension("tmp").exists());
}
