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

## Terminal ecosystem inspiration

- [yazi](https://github.com/sxyazi/yazi), [gitui](https://github.com/gitui-org/gitui),
  [bottom](https://github.com/ClementTsang/bottom),
  [bandwhich](https://github.com/imsnif/bandwhich),
  [trippy](https://github.com/fujiapple852/trippy) — for the "default plugin" trio
  we plan to ship with M2.

## Terminal / TUI design lineage

- [zellij](https://github.com/zellij-org/zellij) and
  [helix](https://github.com/helix-editor/helix) —
  layout tree + modal keymap patterns.
- [alacritty](https://github.com/alacritty/alacritty) — VT parser API shape.

Missing an attribution? Open an issue.
