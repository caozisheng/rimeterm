use std::collections::BTreeMap;
use std::path::PathBuf;

use rimeterm_config::memory_state::{
    ActiveTabsState, MemoryPolicy, MemoryState, PaneState, UiState, memory_policy_file,
    ui_state_file,
};

#[test]
fn memory_policy_defaults_every_stable_category_on() {
    let policy = MemoryPolicy::default();

    assert!(policy.last_workspace);
    assert!(policy.pane_sizes);
    assert!(policy.tab_layout);
    assert!(policy.active_tabs);
    assert!(policy.agent_tabs);
    assert!(policy.shell_tabs);
    assert!(policy.files);
    assert!(policy.git);
    assert!(policy.todo);
    assert!(policy.fast_resume);
    assert!(policy.sysmon);
    assert!(policy.agtop);
    assert!(policy.models);
    assert!(policy.stock);
    assert!(policy.zones);
}

#[test]
fn state_files_are_global_under_data_dir() {
    let root = PathBuf::from("C:/rimeterm-home");

    assert_eq!(memory_policy_file(&root), root.join("data/memory.toml"));
    assert_eq!(ui_state_file(&root), root.join("data/ui.state.toml"));
}

#[test]
fn disabled_categories_are_removed_from_the_saved_snapshot() {
    let mut policy = MemoryPolicy::default();
    policy.pane_sizes = false;
    policy.files = false;
    policy.stock = false;

    let state = UiState {
        last_workspace: Some(PathBuf::from("C:/work/project")),
        pane_sizes: Some(BTreeMap::from([("root".to_owned(), vec![0.4, 0.6])])),
        active_tabs: Some(ActiveTabsState {
            files: Some("todo".to_owned()),
            git: Some("stock".to_owned()),
            agents: 1,
            shells: 2,
        }),
        files: Some(PaneState::default()),
        stock: Some(PaneState::default()),
        ..UiState::default()
    };

    let filtered = state.filtered_by(&policy);

    assert!(filtered.last_workspace.is_some());
    assert!(filtered.pane_sizes.is_none());
    assert!(filtered.active_tabs.is_some());
    assert!(filtered.files.is_none());
    assert!(filtered.stock.is_none());
}

#[test]
fn policy_and_ui_state_round_trip_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy_path = dir.path().join("data/memory.toml");
    let state_path = dir.path().join("data/ui.state.toml");

    let mut policy = MemoryPolicy::default();
    policy.git = false;
    policy.save_to(&policy_path).expect("save policy");
    assert_eq!(
        MemoryPolicy::load_or_default(&policy_path).expect("load policy"),
        policy
    );

    let state = UiState {
        last_workspace: Some(PathBuf::from("/tmp/work")),
        shell_tabs: Some(3),
        models: Some(PaneState {
            values: BTreeMap::from([
                ("sort".to_owned(), "context".to_owned()),
                ("order".to_owned(), "descending".to_owned()),
            ]),
        }),
        ..UiState::default()
    };
    state.save_to(&state_path).expect("save state");
    assert_eq!(
        UiState::load_or_default(&state_path).expect("load state"),
        state
    );
    assert!(!policy_path.with_extension("tmp").exists());
    assert!(!state_path.with_extension("tmp").exists());
}

#[test]
fn memory_state_loads_policy_and_filters_ui_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut policy = MemoryPolicy::default();
    policy.fast_resume = false;
    policy
        .save_to(&memory_policy_file(dir.path()))
        .expect("save policy");
    UiState {
        fast_resume: Some(PaneState::default()),
        zones: Some(PaneState::default()),
        ..UiState::default()
    }
    .save_to(&ui_state_file(dir.path()))
    .expect("save state");

    let loaded = MemoryState::load_from(dir.path()).expect("load memory state");

    assert!(loaded.ui.fast_resume.is_none());
    assert!(loaded.ui.zones.is_some());
}
