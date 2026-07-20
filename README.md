# rimeterm

**A terminal built for coding agents.** TUI-native (ratatui), Rust-first,
Windows-priority, cross-platform.

<img width="2796" height="1664" alt="rimeterm workspace screenshot" src="https://github.com/user-attachments/assets/b80c8187-797f-4d0f-8593-40ce609bb5a1" />

| | |
|---|---|
| **License** | Apache-2.0 · see [`LICENSE`](LICENSE) |
| **CI** | [![CI](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml) Linux · macOS (arm) · Windows |
| **Releases** | [Latest](https://github.com/caozisheng/rimeterm/releases/latest) · builds for `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` |
| **MSRV** | Rust 1.90 (edition 2024) |
| **Status** | v0.1 — under active development |

---

## What it is

rimeterm is a **TUI multiplexer** where **AI coding agents are first-class
citizens**, sitting next to the file manager, the shell, and the system
monitor. Four quadrants, tabs inside each, hot-swappable panes, a scripted
IPC surface (`rimectl`), and a picker that spawns any coding agent you
have on `$PATH` — `omp` / `codex` / `claude` / `pi` today, more later.

It is **not** a terminal emulator competing with wezterm / Alacritty /
Kitty. It runs *inside* your existing terminal and multiplexes PTY
sessions the way tmux / Zellij do — but with an agent-shaped hole in the
middle and a scriptable control channel so tests, hooks, and other agents
can drive the UI.

## Design principles

Excerpt from the internal design contract (§0):

1. **AI-native, not AI-bolted-on** — agent sessions, tool calls, approvals
   are kernel-level concepts, not overlays.
2. **One process, many surfaces** — every pane shares the same render /
   event / config / keymap system.
3. **Hot-pluggable = no belief** — any pane (including the default five
   `yazi` / `gitui` / `bottom` / `bandwhich` / `trippy`) can be disabled,
   replaced, or reloaded. The kernel knows nothing about them beyond a
   contract.
4. **Correctness > performance > polish** — millisecond response is the
   floor, not the pitch.
5. **Windows first-class** — ConPTY, path encoding, keybindings, Nerd
   Font fallback all designed against Windows before being verified on
   Linux / macOS.
6. **Fully open source, no closed-source tail** — every dependency must
   allow free redistribution.
7. **External tools = dependencies. Not bundled, not forked, but
   optionally installable via `cargo install`.** rimeterm probes with
   `which::which` first; user-installed via `winget` / `scoop` / `brew` /
   `apt` always wins. Missing tools fall back to a placeholder pane with
   an install hint. The convenience `cargo install --locked <crate>`
   channel exists for `yazi` / `gitui` / `bottom` / `bandwhich` / `trippy`
   (all `crates.io` crates) so users on platforms without a system
   package manager have a one-command path.
8. **ratatui components first** — every widget already in ratatui or
   maintained third-party crates (`nucleo-matcher`, `ratatui-image`, …)
   MUST be reused before writing a new one.

## Feature snapshot

- **Four-quadrant layout** — `files` (yazi/gitui) · `sysmon`
  (bottom/bandwhich/trippy) · `agents` (dropdown-picked) · `shells`
  (pwsh / bash / fish; multi-tab). Every quadrant has an internal tab
  strip.
- **Draggable dividers** with min-size floors; keyboard resize mode
  (`Ctrl+Alt+R`). Layout ratios persist per-workspace to
  `~/.rimeterm/data/workspaces/<hash>/layout.state.toml`.
- **Mouse everywhere** — click a pane to focus; click a tab to switch;
  click `×` to close; click `[+]` to open a dropdown picker; drag a
  divider to resize; scroll and drag are forwarded to the pane's PTY
  child as xterm SGR mouse sequences (so yazi / htop / omp all work).
  **Right-click** opens a context menu built for the click zone
  (divider / tab / pane / placeholder).
- **Agent picker** — the `agents` quadrant starts empty. `Ctrl+T` or
  `[+]` opens a dropdown of every coding agent detected on `$PATH`;
  missing agents render dim with an install hint. Your pick is
  **persisted** to `~/.rimeterm/data/workspaces/<hash>/agents.state.toml`
  so the next launch reopens the same tab without prompting.
- **`rimectl` IPC** — line-delimited JSON over Windows named pipe /
  Unix socket. Full command registry: `workspace.snapshot`, `.pane.write`,
  `.pane.output`, `.pane.wait` (server-side regex poll), `.pane.open`
  (`shell` or `agent:<id>`), `.pane.close`, `.pane.rename`, `.pane.focus`;
  `tools.list` / `.install` / `.upgrade` / `.uninstall`; `agents.list` +
  four `agents.pick.<id>` palette commands.
- **Command palette** (`F1` or `Ctrl+Shift+P`) — fuzzy search over every
  registered command including the ones IPC exposes.
- **Terminal capability responder** — the vt100 parser doesn't
  synthesize DA/DSR responses (Ink apps like oh-my-pi hang without
  them), so we do it inline in the PTY read loop: `ESC[c` /`ESC[>c` /
  `ESC[5n` / `ESC[6n` all get the right reply.
- **Windows Explorer icon** embedded via `winresource` build script; the
  `.exe` shows the rimeterm logo in taskbar / Alt-Tab / file properties.
- **Storage under `~/.rimeterm/`** — single dot-dir per user (yazi /
  nushell / starship pattern), overridable via `$RIMETERM_HOME`. Project-
  scoped overrides live at `<workspace>/.rimeterm/config.toml`.

## Install

### From a release

Grab the archive for your platform from the [latest release]:

```bash
# Linux / macOS
tar -xzf rimeterm-<version>-<target-triple>.tar.gz
sudo install rimeterm-*/rimeterm rimeterm-*/rimectl /usr/local/bin/

# Windows (PowerShell)
Expand-Archive rimeterm-<version>-x86_64-pc-windows-msvc.zip -DestinationPath $env:LOCALAPPDATA\rimeterm
# then add that dir to PATH
```

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest

### From source

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
```

## External tools (optional, detected on start)

rimeterm doesn't bundle any of these. Install what you use; the rest
gets a placeholder pane with the install hint.

| tool | binary | one-liner (crates.io convenience install) |
|---|---|---|
| yazi (file manager) | `yazi` | `cargo install --locked yazi-fm yazi-cli` |
| gitui | `gitui` | `cargo install --locked gitui` |
| bottom (sysmon) | `btm` | `cargo install --locked bottom` |
| bandwhich (bandwidth) | `bandwhich` | `cargo install --locked bandwhich` — needs admin / `cap_net_raw` |
| trippy (traceroute) | `trip` | `cargo install --locked trippy` |

Agents live off `npm` / `pip` / binary releases — see
`rimectl agents.list` for install hints per entry.

## Build

```bash
cargo check --workspace
cargo run   --bin rimeterm       # launch the terminal
cargo run   --bin probe-shell    # print which shell would be picked
cargo test  --workspace
```

Requires Rust ≥ 1.90 (edition 2024). Windows uses ConPTY (Win10 1809+);
Linux / macOS build the same tree via `portable-pty`.

## Keybindings

| key | action |
|---|---|
| `F10` / `Alt+M` | Toggle app menu (top-left `≡ rimeterm`) |
| `F1` / `Ctrl+Shift+P` | Toggle command palette (both bindings — WT eats the latter by default) |
| `Ctrl+Alt+R` | Toggle keyboard Resize mode |
| `Alt+H/J/K/L` | Focus left / down / up / right cell |
| `Alt+1..4` | Jump to quadrant (files / agents / sysmon / shells) |
| `Ctrl+PgUp/PgDn` or `Alt+[/]` | Previous / next tab in focused group |
| `Alt+Shift+1..9` | Jump directly to tab N in focused group |
| `Ctrl+T` | New tab: shell (`shells` group) or open agent picker (`agents` group) |
| `Ctrl+W` | Close current shell tab |
| `Ctrl+Q` | Quit rimeterm |
| any other key | Forwarded to the focused pane (encoded with proper modifiers + arrows) |

Mouse: click a tab to switch, click `×` to close, click `[+]` to open a
picker; right-click anywhere for a context menu; scroll wheel + drag are
forwarded to the PTY.

## Scripting via `rimectl`

`rimectl` is a shell wrapper for the local IPC socket. Both binaries
ship in every release archive.

```bash
# Find the running rimeterm.
PID=$(rimectl --list-endpoints | tail -1 | grep -oP 'pid=\K\d+')

# Snapshot the layout / tabs / focus.
rimectl --pid $PID workspace.snapshot | jq

# Open a fresh shell tab, name it, drive it, wait for output, close it.
PANE=$(rimectl --pid $PID workspace.pane.open --json '{"kind":"shell"}' \
  | jq -r .result.pane_id)
rimectl --pid $PID workspace.pane.rename \
  --json "{\"pane_id\": $PANE, \"title\": \"build-runner\"}"
rimectl --pid $PID workspace.pane.write \
  --json "{\"pane_id\": $PANE, \"text\": \"cargo test\", \"enter\": true}"
rimectl --pid $PID workspace.pane.wait \
  --json "{\"pane_id\": $PANE, \"pattern\": \"test result:\", \"timeout_ms\": 60000}"
rimectl --pid $PID workspace.pane.close --json "{\"pane_id\": $PANE}"
```

Every command is listed by `rimectl help`. Selected highlights:

- `workspace.pane.open {kind}` — `shell` or `agent:<id>` (where `<id>` ∈
  registry: `omp` / `codex` / `claude` / `pi`).
- `workspace.pane.wait {pane_id, pattern, timeout_ms?<=60000}` —
  server-side regex poll; returns as soon as `pattern` matches or the
  deadline hits.
- `tools.install {name}` — shell-out to
  `cargo install --locked <crates…>` for the five-tool registry;
  300 s hard timeout; returns exit code + captured output.
- `agents.list` — probe all registry entries; returns detected path +
  install hint per agent.

## Configuration

- **User (global)**: `~/.rimeterm/config.toml` (override root with
  `$RIMETERM_HOME`).
- **Project (per repo)**: `<workspace>/.rimeterm/config.toml` — check
  into git so teammates share layout.
- **State**: `~/.rimeterm/data/workspaces/<hash>/layout.state.toml` (split
  ratios) and `agents.state.toml` (which agents to reopen).
- **Cache**: `~/.rimeterm/cache/` (Unicode-width probe).

## Repository layout

```
rimeterm/
├── Cargo.toml                        # workspace
├── crates/
│   ├── rimeterm/                     # `rimeterm` binary + probe-shell
│   ├── rimectl/                      # `rimectl` binary
│   ├── rimeterm-core/                # PaneProvider / EventBus / Command / LayoutTree
│   ├── rimeterm-config/              # TOML config + ~/.rimeterm paths + registries
│   ├── rimeterm-pty/                 # portable-pty + vt100 + shell/agent detection
│   ├── rimeterm-ipc/                 # named-pipe / uds transport + JSON protocol
│   └── rimeterm-tui/                 # ratatui frontend: app loop, panes, palette, picker
├── ACKNOWLEDGEMENTS.md
├── LICENSE
└── .github/workflows/                # CI + Release matrices (Linux · macOS · Windows)
```

## Contributing

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  clippy::correctness`, and `cargo test --workspace` all run in CI on every
  push and PR (Linux + macOS + Windows). Please run them locally before
  opening a PR.
- Rust edition 2024 · MSRV `rust-version = "1.90"` (see root `Cargo.toml`).
- Use `parking_lot::{Mutex, RwLock}` in code that immediately unwraps
  lock results — the codebase's rule is checked by an internal lint.
- Never `Box::leak` to satisfy a lifetime; use `Arc<T>` / owned data /
  `LazyLock`.

## Acknowledgements

Standing on shoulders. Non-exhaustive:

- **[ratatui]** — the terminal UI framework this whole project is built
  around.
- **[portable-pty]** — cross-platform PTY spawning; the ConPTY path in
  particular is invaluable on Windows.
- **[vt100]** — the parser that turns child output into a grid.
- **[crossterm]** — event / raw-mode / colour backend used by both
  ratatui and our input router.
- **[winresource]** — the build-time icon embedder.
- **[yazi], [gitui], [bottom], [bandwhich], [trippy]** — the TUI tools
  rimeterm hosts by default.
- **[Oh-my-pi], [Codex CLI], [Claude Code], [Pi]** — the coding agents
  the picker knows about.
- **[wezterm]** — where `portable-pty` (and a lot of terminal-behavior
  reference) comes from.

See [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md) for the full list with
licenses.

[ratatui]: https://github.com/ratatui-org/ratatui
[portable-pty]: https://github.com/wez/wezterm/tree/main/pty
[vt100]: https://github.com/doy/vt100-rust
[crossterm]: https://github.com/crossterm-rs/crossterm
[winresource]: https://github.com/BenjaminRi/winresource
[yazi]: https://github.com/sxyazi/yazi
[gitui]: https://github.com/gitui-org/gitui
[bottom]: https://github.com/ClementTsang/bottom
[bandwhich]: https://github.com/imsnif/bandwhich
[trippy]: https://github.com/fujiapple852/trippy
[Oh-my-pi]: https://github.com/inflection-ai/oh-my-pi
[Codex CLI]: https://github.com/openai/codex
[Claude Code]: https://docs.anthropic.com/claude/docs/claude-code
[Pi]: https://github.com/inflection-ai/pi
[wezterm]: https://github.com/wez/wezterm

## License

Licensed under the Apache License, Version 2.0.
Unless you explicitly state otherwise, any contribution you intentionally
submit for inclusion in this work shall be licensed as above, without any
additional terms or conditions.
