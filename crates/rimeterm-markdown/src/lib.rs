//! rimeterm-markdown — pure markdown + LaTeX renderer for the Alt+V viewer.
//!
//! Extracted from [`leboiko/markdown-reader`](https://github.com/leboiko/markdown-reader)
//! v1.34.75 (MIT). See [`NOTICE.md`](../NOTICE.md) for attribution and the
//! [design doc](../../../docs/rimterm-markdown-viewer-design.md) for the
//! scope of the extraction.
//!
//! # What this crate does
//!
//! Parses a Markdown string into a [`markdown::DocBlock`] sequence with
//! ready-to-render `ratatui::text::Text` payloads. Fenced code blocks are
//! syntect-highlighted against the caller-selected [`theme::Theme`]. LaTeX
//! math (`$…$` / `$$…$$`) is approximated to Unicode via `markdown::math`.
//! Mermaid fences produce [`markdown::DocBlock::Mermaid`] entries carrying
//! the raw source; [`render_mermaid_to_image`] rasterises that source
//! through `mermaid-rs-renderer` + `usvg`/`resvg`/`tiny-skia` into an
//! [`image::DynamicImage`] ready to hand to
//! `ratatui-image::picker::Picker`. See `src/mermaid.rs` for the pipeline
//! and `docs/rimeterm-mermaind-design.md` for the design analysis.
//!
//! # What this crate does NOT do
//!
//! - **Terminal image protocol** — rendering the returned
//!   [`image::DynamicImage`] to Kitty / Sixel / iTerm2 / half-block cells
//!   is the caller's job (rimeterm's viewer already owns a
//!   `ratatui_image::picker::Picker` for its file-image branch and
//!   reuses it for mermaid diagrams).
//! - **Widget layout / scroll / selection** — no `ratatui::widgets::Widget`
//!   impl. Consumers own the draw pass; upstream's `MarkdownViewState` +
//!   `draw()` were tightly coupled to `crate::app::App` and were not
//!   extractable without a rewrite.
//! - **Tables wide-modal, outline picker, in-doc search** — app-shell
//!   features from upstream, out of scope here.

pub mod cast;
pub mod markdown;
pub mod mermaid;
pub mod section_extract;
pub mod text_layout;
pub mod theme;

pub use crate::markdown::highlight::highlight_code;
pub use crate::markdown::renderer::render_markdown;
pub use crate::markdown::table_layout::layout_table;
pub use crate::markdown::{
    CellSpans, DocBlock, HeadingAnchor, LinkInfo, MermaidBlockId, TableBlock, TableBlockId,
    TextBlockId, cell_to_string, heading_to_anchor,
};
pub use crate::mermaid::{MermaidError, MermaidPixmap, render_mermaid_to_image};
pub use crate::theme::{Palette, Theme};
