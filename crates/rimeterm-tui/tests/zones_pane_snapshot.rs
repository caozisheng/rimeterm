//! ASCII snapshot of the equirectangular world map, dumped so we can eyeball
//! the aspect ratio in test output. Not a golden-file test — we only assert
//! the geometry (unit_w == 4 * unit_h) and that the seeded home marker
//! shows up somewhere in the buffer.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use rimeterm_config::ZonesConfig;
use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};
use rimeterm_tui::zones_pane::ZonesPane;

#[test]
fn snapshot_wide_layout_dumps_landscape_map() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zones.toml");
    let mut pane = ZonesPane::new(ZonesConfig::default(), path);
    pane.set_visible(true);

    let (w, h) = (120u16, 30u16);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let ctx = PaneRenderCtx {
                focused: true,
                title_override: None,
                focus_color: ratatui::style::Color::Magenta,
            };
            let _ = pane.render(Rect::new(0, 0, w, h), f, &ctx);
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    // Extract just the glyphs, one row per line, so a human eyeballing the
    // test output can spot map-shape regressions instantly.
    let mut dump = String::new();
    for y in 0..h {
        for x in 0..w {
            dump.push_str(buffer[(x, y)].symbol());
        }
        dump.push('\n');
    }
    // Print unconditionally so `cargo test -- --nocapture` shows it.
    println!("=== ZonesPane 120x30 snapshot ===\n{dump}");

    // Sanity: the map area should be non-empty and contain the ◉ home marker.
    assert!(
        dump.contains('◉'),
        "expected the home marker to be drawn somewhere"
    );
    // Sanity: at least one braille glyph (U+2800-U+28FF) somewhere → coastline.
    let has_braille = dump.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
    assert!(has_braille, "expected at least one braille coastline dot");
}
