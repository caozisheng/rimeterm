use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rimeterm_core::pane::PaneProvider;
use rimeterm_tui::file_manager_pane::FileManagerPane;
use std::{
    fs,
    time::{Duration, Instant},
};
use tempfile::tempdir;

#[test]
fn paste_request_runs_in_background_and_applies_completion() {
    let left = tempdir().expect("left tempdir");
    let right = tempdir().expect("right tempdir");
    let source = left.path().join("source.txt");
    fs::write(&source, b"payload").expect("write source");
    let mut pane = FileManagerPane::new(left.path().to_path_buf(), right.path().to_path_buf());

    assert!(pane.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)));
    assert!(pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    let started = Instant::now();
    assert!(pane.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)));
    assert!(started.elapsed() < Duration::from_millis(100));

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !pane.poll_background() {
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(
        fs::read(right.path().join("source.txt")).unwrap(),
        b"payload"
    );
}
