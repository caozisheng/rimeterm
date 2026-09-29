//! Rendered snapshots of the single-row top bar at several widths.
//! `App::draw` delegates the whole header row to `top_bar::render`
//! with the same inputs, so these snapshots exercise the exact pixels
//! users see without booting real PTYs.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use rimeterm_config::memory_state::WorkspaceLayoutMode;
use rimeterm_tui::top_bar::{self, StatusBarHover, TopBarInput};
use rimeterm_tui::workspace_strip::{WorkspaceHover, WorkspaceRename};

fn titles() -> Vec<String> {
    vec!["alpha".into(), "beta".into()]
}

fn input(titles: &[String]) -> TopBarInput<'_> {
    TopBarInput {
        titles,
        active: 0,
        workspace_label: "myproj",
        shell_short: "pwsh",
        layout_mode: WorkspaceLayoutMode::Landscape,
        tabs_enabled: true,
        scroll_offset: 0,
        key_hint: None,
    }
}

fn snap(width: u16, input: &TopBarInput<'_>) -> (String, top_bar::TopBarHits) {
    let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
    let mut hits = top_bar::TopBarHits::default();
    term.draw(|f| {
        hits = top_bar::render(
            f.area(),
            f.buffer_mut(),
            input,
            WorkspaceHover::None,
            StatusBarHover::None,
            None,
        );
    })
    .unwrap();
    let text: String = (0..width)
        .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
        .collect();
    (text, hits)
}

#[test]
fn wide_header_snapshot_shows_tabs_left_status_right() {
    let t = titles();
    let (text, hits) = snap(100, &input(&t));
    println!("=== 100 cols ===\n{text}");
    assert!(text.starts_with(" ≡ rimeterm "), "menu leads: {text}");
    let quit = hits.status.quit.expect("quit");
    assert_eq!(quit.x + quit.width, 100);
    assert!(text.contains("workspace: myproj"));
    assert!(text.contains("shell: pwsh"));
    assert!(text.contains("LANDSCAPE"));
    assert!(text.trim_end().ends_with("[×]"));
    // Tabs sit in the left half, status in the right quarter.
    assert!(text.find("alpha").unwrap() < 40);
    assert!(text.find("workspace:").unwrap() > text.find("beta").unwrap());
}

#[test]
fn narrow_header_snapshot_keeps_quit_and_toggles() {
    let t = titles();
    let (text, hits) = snap(34, &input(&t));
    println!("=== 34 cols ===\n{text}");
    assert!(hits.status.quit.is_some(), "quit stays: {text}");
    assert!(hits.status.landscape.is_some());
    assert!(hits.status.vertical.is_some());
    assert!(text.contains(" L "), "compact toggles: {text}");
}

#[test]
fn rename_pill_replaces_title_inside_tab_region() {
    let t = titles();
    let inp = input(&t);
    let rename = WorkspaceRename {
        index: 0,
        buffer: "draft".into(),
    };
    let mut term = Terminal::new(TestBackend::new(100, 1)).unwrap();
    let mut hits = top_bar::TopBarHits::default();
    term.draw(|f| {
        hits = top_bar::render(
            f.area(),
            f.buffer_mut(),
            &inp,
            WorkspaceHover::None,
            StatusBarHover::None,
            Some(&rename),
        );
    })
    .unwrap();
    let text: String = (0..100)
        .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
        .collect();
    println!("=== rename 100 cols ===\n{text}");
    assert!(text.contains("draft"));
    assert!(!text.contains("alpha"));
    assert!(text.contains("beta"));
    let caret = top_bar::rename_caret_col(Rect::new(0, 0, 100, 1), &inp, &rename);
    assert!(
        (caret as u16) < hits.tabs_region.x + hits.tabs_region.width,
        "caret {caret} stays inside tab region {:?}",
        hits.tabs_region
    );
}
