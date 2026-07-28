use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use rimeterm_core::{EventBus, FileSide, KernelEvent, PaneProvider};
use rimeterm_tui::file_manager_pane::FileManagerPane;
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn switching_columns_emits_cwd_and_selection_events() {
    let left = tempdir().expect("left");
    let right = tempdir().expect("right");
    let selected = right.path().join("selected.rs");
    fs::write(&selected, "fn main() {}").expect("write");
    let bus = EventBus::new(8);
    let mut subscriber = bus.subscribe();
    let mut pane =
        FileManagerPane::with_event_bus(left.path().to_path_buf(), right.path().to_path_buf(), bus);

    pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    match subscriber.next().await.unwrap().unwrap() {
        KernelEvent::FileManagerCwdChanged { side, path, .. } => {
            assert_eq!(side, FileSide::Right);
            assert_eq!(path, right.path());
        }
        other => panic!("unexpected first event: {other:?}"),
    }
    match subscriber.next().await.unwrap().unwrap() {
        KernelEvent::FileSelected { side, path, .. } => {
            assert_eq!(side, FileSide::Right);
            assert_eq!(path, selected);
        }
        other => panic!("unexpected second event: {other:?}"),
    }
}
