//! Mermaid → SVG → PNG raster pipeline for [`DocBlock::Mermaid`] blocks.
//!
//! # Why not Unicode box-drawing
//!
//! An earlier iteration wrapped [`mermaid_text`] (Unicode glyphs) as a
//! plaintext renderer. Visual quality was unacceptable past three-node
//! flowcharts because the ASCII/box grid discretises node sizes, edges,
//! and labels — no sub-cell interpolation, no curves. See
//! [`docs/rimeterm-mermaind-design.md`] for the full evaluation.
//!
//! # What this pipeline does
//!
//! [`render_mermaid_to_image`] takes a fenced-block source string and
//! returns a decoded [`image::DynamicImage`] ready to hand to
//! `ratatui-image::picker::Picker::new_protocol`. The steps:
//!
//! 1. **Sanitise** — normalise a handful of Mermaid-JS-only tokens
//!    [`mermaid_rs_renderer`] rejects (`<br/>` → space, `<-->` → `---`,
//!    `x--x` / `o--o` → `---`). Design-borrowed from
//!    [`CleverCloud/mdr`][mdr]`::preprocess_mermaid_source`.
//! 2. **Parse → SVG** via [`mermaid_rs_renderer::render`], wrapped in
//!    [`std::panic::catch_unwind`] because the crate is known to panic
//!    on niche syntax (e.g. `{diamond text}` shapes). A panic surfaces
//!    as [`MermaidError::Panic`] instead of tearing down the TUI.
//! 3. **Parse SVG + shape** via [`usvg::Tree::from_str`] with a
//!    process-wide, [`LazyLock`]-cached [`usvg::fontdb::Database`] so
//!    system-font enumeration happens once (~hundreds of ms first hit,
//!    zero after).
//! 4. **Rasterise** into a [`tiny_skia::Pixmap`] via [`resvg::render`],
//!    guarded by [`MAX_TEXTURE_SIZE`] to avoid unreasonable pixmap
//!    allocations on pathological SVGs.
//! 5. **PNG-encode + decode** through [`image::load_from_memory`] so
//!    the output is the same [`image::DynamicImage`] type the existing
//!    viewer image branch feeds to `ratatui-image`.
//!
//! [`DocBlock::Mermaid`]: crate::markdown::DocBlock::Mermaid
//! [`docs/rimeterm-mermaind-design.md`]: ../../../docs/rimeterm-mermaind-design.md
//! [mdr]: https://github.com/CleverCloud/mdr

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock};

/// Hard cap on either raster dimension (mirrors mdr's rationale: keep
/// GPU/allocator behaviour predictable). SVGs that would exceed the
/// cap after `usvg` reports their natural size are scaled down to fit.
pub const MAX_TEXTURE_SIZE: u32 = 8192;

/// Everything that can go wrong on the mermaid → pixmap path. Callers
/// should render the raw source between `[mermaid]` / `[/mermaid]`
/// sentinel lines on any error so the reader always sees *something*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MermaidError {
    /// [`mermaid_rs_renderer::render`] returned an error (unsupported
    /// diagram keyword, malformed edge, unknown shape …). The inner
    /// string is the crate's own diagnostic.
    Parse(String),
    /// [`mermaid_rs_renderer::render`] panicked. The inner string is
    /// the payload extracted from the unwind, or a fixed message when
    /// the payload was not a `String`.
    Panic(String),
    /// SVG parse, raster, or PNG encode/decode failed. The inner
    /// string names the step and the cause.
    Rasterize(String),
    /// The rendered SVG reported non-positive dimensions.
    EmptySvg,
}

impl std::fmt::Display for MermaidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MermaidError::Parse(msg) => write!(f, "mermaid parse: {msg}"),
            MermaidError::Panic(msg) => write!(f, "mermaid renderer panic: {msg}"),
            MermaidError::Rasterize(msg) => write!(f, "mermaid rasterize: {msg}"),
            MermaidError::EmptySvg => write!(f, "mermaid renderer produced empty SVG"),
        }
    }
}

impl std::error::Error for MermaidError {}

/// Decoded mermaid raster ready to hand to `ratatui-image` /
/// `render_image`. Aspect ratio is exposed alongside the image so the
/// caller can pick a row-height without decoding pixel dimensions.
#[derive(Debug, Clone)]
pub struct MermaidPixmap {
    /// PNG round-tripped `DynamicImage`. Type parity with the existing
    /// viewer image branch means the same `picker.new_protocol` call
    /// works with zero further adaptation.
    pub image: image::DynamicImage,
    /// Rendered pixel width. Equal to `image.width()`; cached here so
    /// callers can compute a display height without a second decode.
    pub width: u32,
    /// Rendered pixel height. Equal to `image.height()`.
    pub height: u32,
}

/// Render a mermaid fenced-block source string into a
/// [`MermaidPixmap`]. Never panics — every failure surface returns a
/// [`MermaidError`].
///
/// The heavy work (font enumeration, SVG parse, raster) happens on the
/// calling thread; there is no internal parallelism. Callers that
/// render several diagrams per document should cache the result per
/// block (e.g. in `DocBlock::Mermaid::rendered`), since re-running the
/// pipeline for the same source is deterministic but not free
/// (typically 5–50 ms per diagram on modern hardware).
pub fn render_mermaid_to_image(source: &str) -> Result<MermaidPixmap, MermaidError> {
    let sanitised = sanitise_source(source);

    // catch_unwind because mermaid_rs_renderer 0.2 is known to panic on
    // some inputs (e.g. `{diamond}` shape). AssertUnwindSafe is fine
    // here — we own the owned String and discard partial state on unwind.
    let render_result =
        std::panic::catch_unwind(AssertUnwindSafe(|| mermaid_rs_renderer::render(&sanitised)));

    let svg = match render_result {
        Ok(Ok(svg)) => svg,
        Ok(Err(err)) => return Err(MermaidError::Parse(err.to_string())),
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else {
                "unknown panic payload".to_string()
            };
            return Err(MermaidError::Panic(msg));
        }
    };

    rasterise_svg(&svg)
}

/// Fixes mermaid-JS-only tokens the pure-Rust `mermaid_rs_renderer`
/// does not recognise. Matches mdr's `preprocess_mermaid_source`
/// verbatim in intent (not source) so bug-parity with the upstream is
/// straightforward to reason about.
fn sanitise_source(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let processed = line
            // HTML line breaks inside node labels → single space.
            .replace("<br/>", " ")
            .replace("<br>", " ")
            .replace("<br />", " ")
            // Bidirectional / crossing edges → plain link. Losing the
            // arrowhead is preferable to a hard parse error since the
            // rendered image still communicates topology.
            .replace("<-->", "---")
            .replace("x--x", "---")
            .replace("o--o", "---");
        out.push_str(&processed);
        out.push('\n');
    }
    out
}

/// SVG → raster. Split out so the pipeline can be tested with a
/// hand-crafted SVG when `mermaid_rs_renderer` output shape drifts.
fn rasterise_svg(svg: &str) -> Result<MermaidPixmap, MermaidError> {
    let mut options = usvg::Options::default();
    options.fontdb = fontdb();

    let tree = usvg::Tree::from_str(svg, &options)
        .map_err(|e| MermaidError::Rasterize(format!("usvg parse: {e}")))?;
    let size = tree.size();
    let (svg_w, svg_h) = (size.width(), size.height());
    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err(MermaidError::EmptySvg);
    }

    // Clamp so no dimension exceeds MAX_TEXTURE_SIZE. Preserve aspect.
    // Never scale up (a small diagram stays at natural size).
    let scale = {
        let scale_w = MAX_TEXTURE_SIZE as f32 / svg_w;
        let scale_h = MAX_TEXTURE_SIZE as f32 / svg_h;
        scale_w.min(scale_h).min(1.0)
    };
    let width = (svg_w * scale).max(1.0) as u32;
    let height = (svg_h * scale).max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| MermaidError::Rasterize("pixmap alloc".into()))?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png = pixmap
        .encode_png()
        .map_err(|e| MermaidError::Rasterize(format!("png encode: {e}")))?;
    let image = image::load_from_memory(&png)
        .map_err(|e| MermaidError::Rasterize(format!("png decode: {e}")))?;

    Ok(MermaidPixmap {
        image,
        width,
        height,
    })
}

/// Process-wide font database. `usvg` needs fonts to lay out `<text>`
/// nodes (labels, edge captions). `load_system_fonts` walks OS font
/// dirs and takes hundreds of milliseconds on a cold cache — we pay
/// that once via [`LazyLock`], then hand out cheap `Arc` clones.
fn fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    DB.clone()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TEXTURE_SIZE, MermaidError, rasterise_svg, render_mermaid_to_image, sanitise_source,
    };

    #[test]
    fn renders_left_right_flowchart() {
        // The README canonical example. If mermaid_rs_renderer's output
        // shape changes upstream, we want that failure surfaced here
        // rather than as a silent regression in the TUI.
        let pixmap =
            render_mermaid_to_image("graph LR\nA[Build] --> B[Test] --> C[Deploy]").expect("ok");
        assert!(pixmap.width > 0);
        assert!(pixmap.height > 0);
        assert_eq!(pixmap.image.width(), pixmap.width);
        assert_eq!(pixmap.image.height(), pixmap.height);
    }

    #[test]
    fn renders_common_diagram_keywords() {
        // `mermaid_rs_renderer` is deliberately forgiving — even
        // nonsense produces a small SVG. What actually matters is that
        // the mainline diagram keywords rasterise end-to-end without
        // panicking. A regression here means our advertised support
        // matrix in the design doc has drifted.
        for src in [
            "graph LR\n  A[Build] --> B[Test]",
            "sequenceDiagram\n  Alice->>Bob: Hi",
            "pie\n  \"A\" : 30\n  \"B\" : 70",
            "classDiagram\n  class A\n  A <|-- B",
            "stateDiagram-v2\n  [*] --> Running\n  Running --> [*]",
        ] {
            let out = render_mermaid_to_image(src);
            assert!(out.is_ok(), "keyword rasterise failed: {src:?} → {out:?}");
        }
    }

    #[test]
    fn sanitise_drops_html_breaks_and_bidirectional_edges() {
        // Design-parity check for the borrowed preprocessor. We keep
        // the transformations symmetric with mdr's so shared bug
        // reports translate directly.
        let out = sanitise_source(
            "graph LR\n  A[Line 1<br/>Line 2] <--> B[C]\n  X --x Y\n  M --o N\n  P <br /> Q\n",
        );
        assert!(!out.contains("<br"), "should strip HTML breaks: {out}");
        assert!(
            !out.contains("<-->"),
            "should collapse bidirectional: {out}"
        );
        assert!(!out.contains("x--x"), "should collapse xx: {out}");
        assert!(!out.contains("o--o"), "should collapse oo: {out}");
        assert!(
            out.contains("A[Line 1 Line 2]"),
            "should join labels: {out}"
        );
    }

    #[test]
    fn zero_sized_svg_surfaces_as_error_not_panic() {
        // `usvg` rejects a zero-sized root at parse time, so the guard
        // in `rasterise_svg` may return `Rasterize` OR `EmptySvg`
        // depending on which layer noticed first. Either is fine — the
        // contract is "never panic, never return an all-zero image".
        let empty = r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"></svg>"#;
        let err = rasterise_svg(empty).expect_err("err");
        assert!(
            matches!(err, MermaidError::EmptySvg | MermaidError::Rasterize(_)),
            "expected EmptySvg or Rasterize, got {err:?}"
        );
    }

    #[test]
    fn rasterise_hand_crafted_svg_produces_png() {
        // Verify the SVG → PNG → DynamicImage step in isolation. Any
        // future change to the `resvg`/`usvg`/`image` versions that
        // breaks this trio will trip here before the mermaid crate.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
                        <rect width="40" height="20" fill="red"/>
                     </svg>"#;
        let pixmap = rasterise_svg(svg).expect("ok");
        assert_eq!(pixmap.width, 40);
        assert_eq!(pixmap.height, 20);
        assert_eq!(pixmap.image.width(), 40);
        assert_eq!(pixmap.image.height(), 20);
    }

    #[test]
    fn max_texture_size_constant_is_sane() {
        // Sanity guard so an accidental zero constant doesn't produce
        // zero-pixel rasters for every diagram in the workspace.
        assert!(MAX_TEXTURE_SIZE >= 1024);
        assert!(MAX_TEXTURE_SIZE <= 16384);
    }
}
