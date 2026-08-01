# Acknowledgements

rimeterm stands on the shoulders of a lot of open-source work. This file
lists the direct upstreams that ship in the binary; transitive deps
pulled in through `cargo` are covered by `Cargo.lock` + each dep's own
license header.

## Rust runtime / language

- [tokio](https://github.com/tokio-rs/tokio) — MIT
- [futures](https://github.com/rust-lang/futures-rs) — MIT / Apache-2.0
- [tracing](https://github.com/tokio-rs/tracing) — MIT

## TUI

- [ratatui](https://github.com/ratatui/ratatui) — MIT
- [crossterm](https://github.com/crossterm-rs/crossterm) — MIT

## PTY / terminal parsing

- [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty) — MIT.
  Cross-platform PTY spawner (ConPTY on Windows, forkpty elsewhere).
- [alacritty_terminal](https://github.com/alacritty/alacritty) — Apache-2.0.
  VT-500 grid + parser powering every PTY pane (replaced `vt100` in C17
  for full alt-screen / SGR / mouse support).

## Config / paths

- [serde](https://github.com/serde-rs/serde) — MIT / Apache-2.0
- [serde_json](https://github.com/serde-rs/json) — MIT / Apache-2.0
- [toml](https://github.com/toml-rs/toml) — MIT / Apache-2.0
- [directories](https://github.com/dirs-dev/directories-rs) — MIT / Apache-2.0
- [which](https://github.com/harryfei/which-rs) — MIT
- [anyhow](https://github.com/dtolnay/anyhow) — MIT / Apache-2.0
- [thiserror](https://github.com/dtolnay/thiserror) — MIT / Apache-2.0
- [parking_lot](https://github.com/Amanieu/parking_lot) — MIT / Apache-2.0.
  Non-poisoning mutexes used across the App main loop and PTY session
  writers.
- [regex](https://github.com/rust-lang/regex) — MIT / Apache-2.0. Used
  by the `AgtopPane` matcher table and by IPC arg validation.
- [tokio-stream](https://github.com/tokio-rs/tokio) — MIT. Async stream
  glue for the redraw / OSC bridges.

## Native system monitor

`SysmonPane` samples system metrics in-process; no external binary
ships. The stack:

- [sysinfo](https://github.com/GuillaumeGomez/sysinfo) — MIT. Cross-platform
  system/CPU/memory/process/network/disk sampler.
- [humansize](https://github.com/LeopoldArkham/humansize) — MIT / Apache-2.0.
  Byte / rate formatter.

## Native AI-agent monitor

`AgtopPane` classifies AI-coding-agent processes (Claude Code, Codex,
Aider, Cursor, Gemini, Goose, …) in-process; no external binary ships.

- [agtop](https://github.com/mbrassey/agtop) — MIT. Upstream `agtop`
  is a standalone binary; we ported the matcher table
  (`src/matchers.rs` @ v2.4.24) into `rimeterm-tui::agtop_matchers`
  so detection works without a separate `cargo install agtop`. The
  rest of the pane (sysinfo-based worker, ratatui table view, filter
  / sort / cursor UX) is rimeterm-native.
- Also uses [sysinfo] and [humansize] from the system-monitor stack
  above.

## Native AI-model catalog

`ModelsPane` browses the [models.dev](https://models.dev) catalog
(~4,000 models across ~85 providers) in-process; no external binary
ships.

- [modelsdev](https://github.com/reyamira/models) — MIT. Upstream
  `modelsdev` is a standalone TUI + CLI binary; we ported the data
  types (`src/data.rs`) + fetch layer (`src/api.rs`) from v0.14.0
  into the `rimeterm-models` crate so browsing works without a
  separate `cargo install modelsdev`. The rest of the pane (blocking
  worker thread, ratatui table view, filter / sort / cursor UX) is
  rimeterm-native and covers only the Models tab — Agents /
  Benchmarks / Status stay in upstream `modelsdev` for users who
  want the full experience.
- [reqwest](https://github.com/seanmonstar/reqwest) — MIT / Apache-2.0.

## Native stock market pane

`StockPane` renders the three-column A / HK / US stock tab in-process; no
external binary ships.

- [akshare-rs](https://github.com/Cricle/akshare-rs) — MIT / Apache-2.0.
  Pure-Rust port of the upstream Python `akshare` client. Pinned at commit
  `e7291600ab99ee95e6b38b0ce1301154d6eb46d8` (v0.1.14) via a Cargo `git`
  dep with the `mod-stock` / `mod-index` / `mod-news` features. The
  `rimeterm-stock` crate wraps `AkShareClient` to serve A-share, HK, and
  US search, quotes, historical candles, fundamentals (PE / PB / market
  cap), watchlist enrichment, indices (沪深重要指数 / HSI / DJIA-SPX-NDX),
  and Eastmoney-sourced news. Optional `stock.http_proxy` and
  `stock.tushare_token` config knobs plumb through to akshare's proxy
  builder and the Tushare Pro A-share fundamentals fallback. The rest of
  the pane (blocking worker on a dedicated OS thread, ratatui three-column
  layout, filter / sort / cursor UX) is rimeterm-native.

## Native session search

`FR` is an in-process search and conversation-preview pane backed by:

- [fast-resume](https://github.com/angristan/fast-resume) — MIT. The complete
  source at commit `66e42cfd34bca4800161098d3b302a35a52ce69b` is vendored in
  `crates/fast-resume`; rimeterm adds an embedding API and native pane while
  retaining the upstream adapters, Tantivy index, query parser, and `fr` CLI.

## Native files / git stack

Files and Git are Native panes compiled into rimeterm — no external
processes. The stack:

- [tui-file-explorer](https://github.com/sorinirimies/tui-file-explorer) — MIT.
  Two-pane keyboard-driven explorer used by `FileManagerPane` (via the
  `caozisheng/tui-file-explorer` fork adding `draw_in` + deferred
  mutation APIs).
- [gix](https://github.com/GitoxideLabs/gitoxide) / [gix-diff] — MIT /
  Apache-2.0. Pure-Rust Git implementation powering `GitPane`.
- [serie](https://github.com/lusingander/serie) — MIT. Two-pass commit-graph
  layout algorithm (`src/graph/calc.rs` @ commit
  `9488f60ff509620513b2128a1acc14abf4786bbd`) ported into
  `rimeterm-tui::git_worker::{assign_columns,build_edges}`; rimeterm draws
  the resulting edges as Unicode box-drawing glyphs instead of the upstream
  kitty / iTerm2 image-protocol tiles, so the Commits list works in every
  terminal (Windows Terminal, WezTerm, kitty, etc.).
- [tree-sitter](https://tree-sitter.github.io/tree-sitter/) +
  [tree-sitter-highlight] — MIT. Incremental parser used for diff syntax
  highlighting. Grammars: `tree-sitter-{rust,c,cpp,python,javascript,typescript,json,toml-ng,yaml,bash}` (all MIT).

**Extension slot (plugins, on-demand):**
- [trippy](https://github.com/fujiapple852/trippy) — MIT / Apache-2.0.
  Not bundled; installed on demand into `~/.rimeterm/plugins/trippy/`
  via `cargo install --root` when the user runs `tools.install trippy`.

## Modal viewer (Alt+V)

`Viewer` overlays markdown / code / image previews on top of the left
column when the user Alt+V's a file-manager selection.

- [`rimeterm-markdown`](crates/rimeterm-markdown/) — MIT. In-tree fork of
  [leboiko/markdown-reader](https://github.com/leboiko/markdown-reader)
  v1.34.75; carries the pure-Rust Markdown + Mermaid + LaTeX renderer.
  Attribution + fork history in `crates/rimeterm-markdown/NOTICE.md`.
- [tui-markdown](https://github.com/joshka/tui-markdown) — MIT. Ratatui
  block/span translation used inside `rimeterm-markdown`.
- [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) — MIT.
  Zero-copy Markdown tokenizer.
- [ratatui-image](https://github.com/benjajaja/ratatui-image) — MIT.
  Kitty / Sixel / iTerm2 / half-block image renderer for the image
  branch of the viewer.
- [image](https://github.com/image-rs/image) — MIT / Apache-2.0.
  Decoder backend for PNG / JPG / GIF / BMP / WebP.
- [syntect](https://github.com/trishume/syntect) — MIT. Sublime Text
  grammar engine powering the viewer's code-highlight branch.
- [unicode-width](https://github.com/unicode-rs/unicode-width) —
  MIT / Apache-2.0. Grapheme-cluster width tables shared across every
  layout / truncation call site.

## Clipboard / OS integration

- [arboard](https://github.com/1Password/arboard) — MIT / Apache-2.0.
  System clipboard reader / writer used by the mouse-selection copy
  path and the "Copy path" context menu.

## Optional runtime deps

These are compiled in unconditionally so the same binary supports every
host, but activate at runtime only when their host feature is present.
A missing library / daemon silently degrades to "no data" rather than
failing startup.

- [nvml-wrapper](https://github.com/rust-nvml/nvml-wrapper) — MIT.
  NVIDIA NVML dynamic-loader used by `SysmonPane`'s GPU section.
  Missing driver → GPU rows hidden.
- [bollard](https://github.com/fussybeaver/bollard) — Apache-2.0.
  Docker Engine API client used by `SysmonPane`'s Docker container
  count. Missing daemon → Docker row hidden.
- [procfs](https://github.com/eminence/procfs) — MIT / Apache-2.0 (Linux
  only). `/proc/self/cgroup` decoder for the cgroup badge on Linux.

## Todo

- [tuxedo](https://github.com/webstonehq/tuxedo) — MIT. The complete source
  at commit `8c990c0e1f57462115c0d2dffdfffb3f0b63b7db` is vendored in
  `crates/tuxedo`; rimeterm adds a bounded embedding API and native Todo pane
  while retaining the upstream todo.txt engine, TUI, CLI, and standalone
  `tuxedo` binary.

## Terminal / TUI design lineage

- [zellij](https://github.com/zellij-org/zellij) and
  [helix](https://github.com/helix-editor/helix) —
  layout tree + modal keymap patterns.
- [alacritty](https://github.com/alacritty/alacritty) — VT parser API shape.

Missing an attribution? Open an issue.
