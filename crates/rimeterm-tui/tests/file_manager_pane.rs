use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};
use rimeterm_tui::file_manager_pane::{FileManagerPane, FileSide};
use std::fs;
use tempfile::tempdir;

#[test]
fn tab_switches_active_side_without_changing_directories() {
    let root = tempdir().expect("tempdir");
    let mut pane = FileManagerPane::new(root.path().to_path_buf(), root.path().to_path_buf());

    assert_eq!(pane.active_side(), FileSide::Left);
    let left = pane.active_dir().to_path_buf();
    assert!(pane.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    assert_eq!(pane.active_side(), FileSide::Right);
    assert_eq!(pane.active_dir(), left);
}

#[test]
fn highlighted_path_tracks_active_explorer_cursor() {
    let root = tempdir().expect("tempdir");
    let file = root.path().join("main.rs");
    fs::write(&file, "fn main() {}").expect("write file");
    let pane = FileManagerPane::new(root.path().to_path_buf(), root.path().to_path_buf());

    assert_eq!(pane.highlighted_path(), Some(file.as_path()));
}

#[test]
fn render_is_confined_to_pane_rectangle() {
    let root = tempdir().expect("tempdir");
    let mut pane = FileManagerPane::new(root.path().to_path_buf(), root.path().to_path_buf());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    let area = ratatui::layout::Rect::new(8, 3, 60, 18);

    terminal
        .draw(|frame| {
            pane.render(
                area,
                frame,
                &PaneRenderCtx {
                    focused: true,
                    title_override: None,
                    focus_color: ratatui::style::Color::Cyan,
                },
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), " ");
    assert_eq!(buffer[(79, 23)].symbol(), " ");
    assert_ne!(buffer[(area.x, area.y)].symbol(), " ");
}
