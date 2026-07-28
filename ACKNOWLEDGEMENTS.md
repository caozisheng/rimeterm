# Acknowledgements

rimeterm stands on the shoulders of a lot of open-source work. This file lists
the direct upstreams that make the M0 skeleton possible; deeper attributions
will be added as new subsystems come online.

## Rust runtime / language

- [tokio](https://github.com/tokio-rs/tokio) — MIT
- [futures](https://github.com/rust-lang/futures-rs) — MIT / Apache-2.0
- [tracing](https://github.com/tokio-rs/tracing) — MIT

## TUI

- [ratatui](https://github.com/ratatui/ratatui) — MIT
- [crossterm](https://github.com/crossterm-rs/crossterm) — MIT

## PTY / terminal parsing

- [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty) — MIT
- [vt100](https://github.com/doy/vt100-rust) — MIT

## Config / paths

- [serde](https://github.com/serde-rs/serde) — MIT / Apache-2.0
- [toml](https://github.com/toml-rs/toml) — MIT / Apache-2.0
- [directories](https://github.com/dirs-dev/directories-rs) — MIT / Apache-2.0
- [which](https://github.com/harryfei/which-rs) — MIT
- [anyhow](https://github.com/dtolnay/anyhow) — MIT / Apache-2.0
- [thiserror](https://github.com/dtolnay/thiserror) — MIT / Apache-2.0

## Bundled essentials

rimeterm's release archive **bundles prebuilt binaries** for the
following tools so first launch works with no extra installs. All
are permissively licensed. Pinned versions live at
[`essentials/VERSIONS.toml`](essentials/VERSIONS.toml).

- [bottom](https://github.com/ClementTsang/bottom) — MIT. System monitor
  (pinned first tab in the shells column).

## Native files / git stack

Files and Git are Native panes compiled into rimeterm — no external
processes. The stack:

- [tui-file-explorer](https://github.com/sorinirimies/tui-file-explorer) — MIT.
  Two-pane keyboard-driven explorer used by `FileManagerPane` (via the
  `caozisheng/tui-file-explorer` fork adding `draw_in` + deferred
  mutation APIs).
- [gix](https://github.com/GitoxideLabs/gitoxide) / [gix-diff] — MIT /
  Apache-2.0. Pure-Rust Git implementation powering `GitPane`.
- [tree-sitter](https://tree-sitter.github.io/tree-sitter/) +
  [tree-sitter-highlight] — MIT. Incremental parser used for diff syntax
  highlighting. Grammars: `tree-sitter-{rust,c,cpp,python,javascript,typescript,json,toml-ng,yaml,bash}` (all MIT).

**Extension slot (plugins, on-demand):**
- [trippy](https://github.com/fujiapple852/trippy) — MIT / Apache-2.0.
  Not bundled; installed on demand into `~/.rimeterm/plugins/trippy/`
  via `cargo install --root` when the user runs `tools.install trippy`.

## Terminal / TUI design lineage

- [zellij](https://github.com/zellij-org/zellij) and
  [helix](https://github.com/helix-editor/helix) —
  layout tree + modal keymap patterns.
- [alacritty](https://github.com/alacritty/alacritty) — VT parser API shape.

Missing an attribution? Open an issue.
