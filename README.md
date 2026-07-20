# rimeterm

A terminal built for coding agents. TUI-native (ratatui), Rust-first,
plugin-hosted, Windows-priority.

- Design: [`docs/rimeterm-overall-design.md`](docs/rimeterm-overall-design.md)
- Acknowledgements: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0 (see [`LICENSE`](LICENSE))

## Status

<details open>
<summary>📸 Screenshot</summary>
<img width="3795" height="2029" alt="image" src="[https://github.com/user-attachments/assets/116bf358-e8bb-4b0a-a3dd-c553a5a86222](https://github.com/user-attachments/assets/86b621bb-0c33-4ad7-a211-298da39ce7df)" />
</details>

## Build

```
cargo check --workspace
cargo run --bin rimeterm       # launch the terminal
cargo run --bin probe-shell    # print which shell would be picked
cargo test  --workspace
```

Requires Rust ≥ 1.90 (edition 2024). Windows dev target uses ConPTY (Win10
1809+); Linux / macOS build the same tree via `portable-pty`.

## Keys (M3)

| Key                     | Action                                              |
| ----------------------- | --------------------------------------------------- |
| `F10` / `Alt+M`         | Toggle app menu (top-left `≡ rimeterm`)             |
| `Ctrl+Shift+P`          | Toggle command palette                              |
| `Ctrl+Alt+R`            | Toggle keyboard Resize mode                         |
| `H` / `L` / `K` / `J`   | (in Resize mode) adjust focused cell's seam         |
| `Shift+H/L/K/J`         | (in Resize mode) same, 5-cell step                  |
| `=` / `0`               | (in Resize mode) reset focused cell / reset all     |
| `Esc` / `Enter`         | (in Resize mode) exit                               |
| Mouse drag on a seam    | Move that divider live                              |
| `Alt+H/J/K/L`           | Focus left / down / up / right cell                 |
| `Alt+1..4`              | Jump to a quadrant (files / agents / sysmon / shells)|
| `Alt+[` / `Alt+]`       | Previous / next tab in the focused group            |
| `Alt+Shift+1..9`        | Jump directly to tab N in the focused group         |
| `Ctrl+T`                | New shell tab (only accepted by the `shells` group) |
| `Ctrl+W`                | Close current shell tab                             |
| Palette `workspace.layout.reset` | Restore default ratios + delete persisted state |
| `Ctrl+Q`                | Quit rimeterm                                       |
| any other key           | Forwarded to the focused pane                       |

## Repository layout

```
rimeterm/
├── Cargo.toml                        # workspace
├── crates/
│   ├── rimeterm/                     # binary + probe-shell helper
│   ├── rimeterm-core/                # PaneProvider / EventBus / Command / AppMenu
│   ├── rimeterm-config/              # TOML config + XDG/APPDATA paths
│   ├── rimeterm-pty/                 # portable-pty + vt100 + shell detection
│   └── rimeterm-tui/                 # ratatui frontend: status bar, menu, pane, app loop
├── docs/rimeterm-overall-design.md   # THE design contract (start here)
├── ACKNOWLEDGEMENTS.md
└── LICENSE
```

## Where to look next

- **Design intent → code**: every subsystem in the design doc names the crate
  it lives in. Start reading at [§0 Design Principles] and [§19 Default
  Workspace Layout].
- **Adding a command / menu item**: register via
  `rimeterm_core::command::CommandRegistry::register`, then optionally append
  an `AppMenuItem` in `crates/rimeterm-core/src/app_menu.rs`.
- **New PTY pane**: implement `rimeterm_core::pane::PaneProvider`, wire it in
  `App::new`; see `crates/rimeterm-tui/src/pty_pane.rs` for reference.

[§0 Design Principles]: docs/rimeterm-overall-design.md#0-设计原则design-principles
[§19 Default Workspace Layout]: docs/rimeterm-overall-design.md#19-附录-c默认工作区布局default-workspace-layout
