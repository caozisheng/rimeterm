# rimeterm-markdown — Attribution

This crate is derived from
[`leboiko/markdown-reader`](https://github.com/leboiko/markdown-reader)
`@ v1.34.75` (commit at extraction time).

Original MIT license retained verbatim in [`LICENSE`](./LICENSE).

## Modifications

- **Stripped** the application shell: `src/app/`, `src/ui/{tab_bar,
  tab_picker, tabs, file_tree, help, copy_menu, editor,
  hybrid_editor, link_picker, doc_search_bar, goto_line_bar,
  config_popup, mermaid_modal, search_modal, outline_picker,
  status_bar, table_modal, table_render, layout}.rs`, `src/fs/`,
  `src/checklinks/`, `src/export/`, `src/version_check/`,
  `src/config.rs`, `src/main.rs`, `src/event.rs`, `src/action.rs`,
  `src/cast.rs`, `src/state.rs`, `src/markdown/cursor_bridge.rs`.
- **Inlined** the `MermaidMode` and `MermaidTextBackend` enums
  (previously in `crate::config`) into `src/mermaid.rs` to sever the
  last coupling to the upstream config module.
- **Renamed** the `src/ui/markdown_view/` module to `src/view/` —
  there is no `ui` layer in this crate; the widget IS the top level.
- **Added** a `MarkdownViewWidget` newtype implementing
  `ratatui::widgets::Widget` around the upstream `MarkdownViewState`
  free-function draw path, so consumers can plug the renderer into a
  standard ratatui composition chain.
- **Replaced** the upstream integration test suite (`src/ui/
  markdown_view/tests.rs`, 1163 LoC referencing `crate::app`
  fixtures) with focused unit tests under `tests/` covering the four
  invariants named in the design doc.
- **Added** `src/mermaid.rs` — a mermaid → SVG → PNG raster pipeline
  wrapping [`mermaid-rs-renderer`](https://github.com/1jehuang/mermaid-rs-renderer)
  (MIT), [`usvg`](https://github.com/linebender/resvg) / [`resvg`](https://github.com/linebender/resvg) (MIT / Apache-2.0), and
  [`tiny-skia`](https://github.com/linebender/tiny-skia) (BSD-3-Clause). Design shape (preprocess → `catch_unwind`
  → rasterise → PNG round-trip → `image::DynamicImage`) is borrowed
  from [CleverCloud/mdr](https://github.com/CleverCloud/mdr) v0.3.2 (MIT) `src/core/mermaid.rs`; the
  implementation is independent. See `docs/rimeterm-mermaind-design.md`
  for the evaluation that led here.

## Divergence policy

Ongoing changes are rimeterm-owned. Upstream is a **source of ideas**
for cherry-picks, not an automatic merge target. See
`docs/rimterm-markdown-viewer-design.md` for the manual re-vendor
procedure.
