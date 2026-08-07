use glab_tui::embed::{CachePolicy, EmbeddedFeatures, EmbeddedOptions};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::path::PathBuf;

fn make_options() -> EmbeddedOptions {
    EmbeddedOptions {
        workspace_root: PathBuf::from("C:/fixture/repo"),
        initial_tab: None,
        cache_policy: CachePolicy::Manual,
        refresh: None,
        features: EmbeddedFeatures::default(),
        glab_config: None,
    }
}

#[test]
fn draw_in_does_not_touch_cells_outside_area() {
    let handle = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .handle()
        .clone();
    let mut app = glab_tui::embed::EmbeddedApp::new(make_options(), handle);

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    // Fill entire buffer with sentinel
    terminal
        .draw(|frame| {
            let buf = frame.buffer_mut();
            for y in 0..buf.area().height {
                for x in 0..buf.area().width {
                    buf[(x, y)].set_char('░');
                }
            }
            // Render only into a sub-rect
            let inner = Rect::new(5, 3, 50, 14);
            app.render(frame, inner);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    // Corners outside the render area must retain the sentinel
    assert_eq!(buf[(0, 0)].symbol(), "░", "top-left sentinel changed");
    assert_eq!(buf[(59, 19)].symbol(), "░", "bottom-right sentinel changed");
    assert_eq!(
        buf[(4, 2)].symbol(),
        "░",
        "just outside left-top sentinel changed"
    );
}
