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

## Bundled essentials (C21.5)

rimeterm's release archive **bundles prebuilt binaries** for the
following tools so first launch works with no extra installs. All
are permissively licensed and redistribution-friendly. Pinned
versions live at [`essentials/VERSIONS.toml`](essentials/VERSIONS.toml);
bump per rimeterm release.

**Quadrant tools:**
- [yazi](https://github.com/sxyazi/yazi) — MIT. File manager (files quadrant).
- [gitui](https://github.com/gitui-org/gitui) — MIT. Git TUI.
- [bottom](https://github.com/ClementTsang/bottom) — MIT. System monitor.

**Yazi Quick Look previewers:**
- [bat](https://github.com/sharkdp/bat) — MIT / Apache-2.0.
  Syntax-highlighted text/code preview (Yazi's default text handler).
- [glow](https://github.com/charmbracelet/glow) — MIT. Markdown preview.
- [chafa](https://hpjansson.org/chafa/) — LGPL-3.0. Image preview
  fallback (used when the terminal doesn't support Kitty / iTerm2 /
  Sixel graphics protocols). Rehosted under the `essentials-mirror`
  release tag because upstream doesn't publish to GitHub Releases;
  `aarch64-apple-darwin` is skipped since upstream has no macOS
  build (macOS terminals almost always support Kitty or iTerm2 image
  protocols so chafa isn't needed there).

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
