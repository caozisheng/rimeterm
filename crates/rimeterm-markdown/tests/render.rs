//! Extraction acceptance tests (C22.6). Locks the four rendering
//! invariants named in `docs/rimterm-markdown-viewer-design.md`:
//!
//! 1. Empty input → 1 empty text block (no panic, no spurious mermaid).
//! 2. Fenced code block acquires syntect styling.
//! 3. Mermaid fence → `DocBlock::Mermaid { id, .. }` with stable hash.
//! 4. LaTeX `$x^2$` → `x²` Unicode superscript.

use rimeterm_markdown::{DocBlock, Palette, Theme, render_markdown};

fn palette() -> Palette {
    Palette::from_theme(Theme::Default)
}

#[test]
fn empty_input_produces_no_blocks_or_a_single_empty_text_block() {
    let blocks = render_markdown("", &palette(), Theme::Default);
    // Upstream contract: at most one Text block with zero lines. Never
    // panics, never emits a Mermaid entry.
    assert!(blocks.len() <= 1);
    for b in &blocks {
        assert!(!matches!(b, DocBlock::Mermaid { .. }));
    }
}

#[test]
fn fenced_code_block_gets_syntect_style() {
    let src = "```rust\nfn main() {}\n```\n";
    let blocks = render_markdown(src, &palette(), Theme::Default);
    let text_block = blocks
        .iter()
        .find_map(|b| match b {
            DocBlock::Text { text, .. } => Some(text),
            _ => None,
        })
        .expect("expected a Text block");
    // At least one span should have a non-default style — syntect
    // highlighting has kicked in.
    let has_style = text_block
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .any(|s| s.style != ratatui::style::Style::default());
    assert!(has_style, "expected syntect styling inside code block");
}

#[test]
fn mermaid_fence_produces_mermaid_block_with_stable_id() {
    let src = "```mermaid\ngraph LR\nA-->B\n```\n";
    let blocks_a = render_markdown(src, &palette(), Theme::Default);
    let blocks_b = render_markdown(src, &palette(), Theme::Default);

    let id_a = blocks_a
        .iter()
        .find_map(|b| match b {
            DocBlock::Mermaid { id, .. } => Some(*id),
            _ => None,
        })
        .expect("expected a Mermaid block");
    let id_b = blocks_b
        .iter()
        .find_map(|b| match b {
            DocBlock::Mermaid { id, .. } => Some(*id),
            _ => None,
        })
        .expect("expected a Mermaid block on second render");
    assert_eq!(id_a, id_b, "Mermaid ID must be a stable hash of the source");
}

#[test]
fn inline_latex_math_approximates_to_unicode() {
    // `$x^2$` → `x²` per markdown::math::latex_to_unicode.
    let src = "prose $x^2$ tail\n";
    let blocks = render_markdown(src, &palette(), Theme::Default);
    let rendered: String = blocks
        .iter()
        .filter_map(|b| match b {
            DocBlock::Text { text, .. } => Some(text),
            _ => None,
        })
        .flat_map(|t| t.lines.iter())
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        rendered.contains("x²"),
        "expected Unicode superscript, got {rendered:?}",
    );
}
