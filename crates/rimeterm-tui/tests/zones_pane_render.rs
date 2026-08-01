//! Smoke-test the ZonesPane renderer against a small offscreen buffer, at every
//! layout tier the design promises. Guards against out-of-bounds panics if a
//! future tweak drops one of the size guards.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use rimeterm_config::ZonesConfig;
use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};
use rimeterm_tui::zones_pane::ZonesPane;

fn pane() -> ZonesPane {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zones.toml");
    let mut pane = ZonesPane::new(ZonesConfig::default(), path);
    pane.set_visible(true);
    pane
}

fn render_at(width: u16, height: u16) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut p = pane();
    terminal
        .draw(|f| {
            let ctx = PaneRenderCtx {
                focused: true,
                title_override: None,
                focus_color: ratatui::style::Color::Magenta,
            };
            let _ = p.render(Rect::new(0, 0, width, height), f, &ctx);
        })
        .unwrap();
}

#[test]
fn renders_tiny_area_without_panic() {
    render_at(20, 6);
}

#[test]
fn renders_compact_layout() {
    render_at(50, 10);
}

#[test]
fn renders_standard_layout() {
    render_at(70, 20);
}

#[test]
fn renders_wide_layout_with_side_list() {
    render_at(120, 30);
}

#[test]
fn renders_degenerate_zero_size() {
    // The pane must handle a 0×0 area (transient during layout drag).
    render_at(0, 0);
}
