# rimeterm

A terminal built for coding agents. TUI-native (ratatui), Rust-first,
plugin-hosted, Windows-priority.

- Design: [`docs/rimeterm-overall-design.md`](docs/rimeterm-overall-design.md)
- Acknowledgements: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0 (see [`LICENSE`](LICENSE))

## Status

**C14 agent picker — empty by default, dropdown on `Ctrl+T`** — the
`agents` quadrant starts truly empty on first launch. Pressing `Ctrl+T`
inside it opens the command palette pre-filtered to `agents.pick.`, so
you get a live dropdown of coding agents rimeterm just probed on your
`$PATH`. Nothing is spawned until you pick one — matches the interjection
intent exactly.

```
PID=$(rimectl --list-endpoints | tail -1 | grep -oP 'pid=\K\d+')

# Which agents are actually installed?
rimectl --pid $PID agents.list \
  | jq -r '.result.agents[] | "\(.id)\t\(.detected_path // "-")"'

# Pick one from the terminal side of the same interface:
rimectl --pid $PID workspace.pane.open --json '{"kind":"agent:codex"}'

# Unknown ids give a structured error listing the valid ones.
rimectl --pid $PID workspace.pane.open --json '{"kind":"agent:nope"}'
```

**Also in C14 (path layout interjection):** everything user-writable now
lives under **`~/.rimeterm/`** — `config.toml`, `data/run/*.pid`
lockfiles, future caches all sit in a single dot-dir instead of the old
`%APPDATA%\rimeterm\` / `$XDG_CONFIG_HOME` / `~/Library/…` split. Repo
scope stays at `<workspace>/.rimeterm/config.toml`. `$RIMETERM_HOME`
overrides the root for CI or multi-profile setups.

Everything C7–C13 keeps working: `workspace.snapshot` / `.pane.write` /
`.pane.output` / `.pane.wait` / `.pane.open` (now `shell` OR
`agent:<id>`) / `.pane.close` / `.pane.rename` / `.pane.focus`;
`tools.list` / `.install` / `.upgrade` / `.uninstall`; new
`agents.list` and four `agents.pick.<id>` palette commands.

**Coming next (C15 candidates):** Settings → Tools/Agents panel
(surface the two registries in a ratatui `List`) · OSC 1337 bridge
(§5.5) · `layout.state.toml` differential storage · `viewer` pane
landing as yazi previewer proxy · deeper agent integration (`@`-mention
cross-tab, approval flow, tool calls).

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
