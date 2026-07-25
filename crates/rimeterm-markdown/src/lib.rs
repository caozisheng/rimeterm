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
//! the raw source — text-only rendering is the caller's responsibility
//! (invoke the sibling `mermaid-text` crate from crates.io if desired).
//!
//! # What this crate does NOT do
//!
//! - **Mermaid image protocol** — the raster path (`mermaid-rs-renderer` +
//!   `resvg` + Kitty/Sixel/iTerm2) is deliberately not vendored to keep
//!   the crate free of `App` coupling. Callers that want image-protocol
//!   Mermaid should render `DocBlock::Mermaid { source, .. }` themselves.
//! - **Widget layout / scroll / selection** — no `ratatui::widgets::Widget`
//!   impl. Consumers own the draw pass; upstream's `MarkdownViewState` +
//!   `draw()` were tightly coupled to `crate::app::App` and were not
//!   extractable without a rewrite.
//! - **Tables wide-modal, outline picker, in-doc search** — app-shell
//!   features from upstream, out of scope here.

pub mod cast;
pub mod markdown;
pub mod section_extract;
pub mod text_layout;
pub mod theme;

pub use crate::markdown::highlight::highlight_code;
pub use crate::markdown::renderer::render_markdown;
pub use crate::markdown::{
    CellSpans, DocBlock, HeadingAnchor, LinkInfo, MermaidBlockId, TableBlock, TableBlockId,
    TextBlockId, cell_to_string, heading_to_anchor,
};
pub use crate::theme::{Palette, Theme};
