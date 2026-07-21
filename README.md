# rimeterm

**A terminal built for coding agents.** TUI-native (ratatui), Rust-first,
Windows-priority, cross-platform.

<img width="2796" height="1664" alt="rimeterm workspace screenshot" src="https://github.com/user-attachments/assets/b80c8187-797f-4d0f-8593-40ce609bb5a1" />

| | |
|---|---|
| **License** | Apache-2.0 · see [`LICENSE`](LICENSE) |
| **CI** | [![CI](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml) Linux · macOS (arm) · Windows |
| **Releases** | [Latest](https://github.com/caozisheng/rimeterm/releases/latest) · archives (`.tar.gz` / `.zip`) for every target plus native installers (`.msi` / `.deb` / `.pkg`), all bundling the essentials sibling. |
| **MSRV** | Rust 1.90 (edition 2024) |
| **Status** | v0.1.3 released (C19 Settings overlay + C20 Alt+V Viewer + C21.5 essentials/plugins split — bundled yazi/gitui/bottom in the release archive, `~/.rimeterm/{bin,plugins}` managed dirs, materialized Yazi bridge). Design docs live in `docs/rimeterm-overall-design.md` (local-only) |

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
3. **Hot-pluggable = no belief** — any pane (including the default four
   `yazi` / `gitui` / `bottom` / `trippy`) can be disabled, replaced, or
   reloaded. The kernel knows nothing about them beyond a contract.
4. **Correctness > performance > polish** — millisecond response is the
   floor, not the pitch.
5. **Windows first-class** — ConPTY, path encoding, keybindings, Nerd
   Font fallback all designed against Windows before being verified on
   Linux / macOS.
6. **Fully open source, no closed-source tail** — every dependency must
   allow free redistribution.
7. **Three essentials bundled; everything else opt-in.** rimeterm's
   release archive ships prebuilt `yazi` / `gitui` / `bottom` (the
   three tools required for the default four-quadrant experience);
   first launch extracts them to `~/.rimeterm/bin/` alongside curated
   configs under `~/.rimeterm/{yazi,gitui,bottom}/`. Every other
   tool — `trippy` today, whatever the user adds tomorrow — is
   opt-in via `tools.install`, installed into
   `~/.rimeterm/plugins/<name>/` via `cargo install --root`. Detection
   probes `~/.rimeterm/bin/` → `~/.rimeterm/plugins/*/bin/` → `$PATH`;
   Upgrade/Uninstall buttons only ever touch the plugin dir so a
   user's own `~/.cargo/bin/` is never at risk. External shells outside
   rimeterm see nothing new — we only mutate PTY-child env.
8. **ratatui components first** — every widget already in ratatui or
   maintained third-party crates (`nucleo-matcher`, `ratatui-image`, …)
   MUST be reused before writing a new one.

## Feature snapshot

- **Four-quadrant layout** — `files` (yazi/gitui) · `sysmon`
  (bottom/trippy) · `agents` (dropdown-picked) · `shells` (pwsh / bash /
  fish; multi-tab). Every quadrant has an internal tab strip.
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
- **Native Settings overlay** — Tools / Agents tabs expose registry status,
  install / upgrade / uninstall actions, refresh, and detected-agent
  launch without dropping to `rimectl`. C19 is functionally wired; visible
  per-row actions, disabled states, mouse handling, and end-to-end coverage
  remain acceptance work. **C21.5 (planned)** splits the tools view into
  essentials (yazi/gitui/bottom — bundled, upgraded with rimeterm) and
  plugins (trippy + user-added — cargo installed on demand into
  `~/.rimeterm/plugins/`); Upgrade / Uninstall buttons are gated on the
  plugin channel only, closing the "we might nuke your personal cargo
  install" hole.
- **Viewer overlay** (`Alt+V`) — Yazi's native third column keeps its
  MIME/plugin Quick Look. `Alt+V` freezes the last Yazi selection into a
  centered Modal Snapshot: Markdown via `tui-markdown`, images via
  `ratatui-image`. The overlay never enters the layout tree, never resizes
  PTYs, and never mirrors Yazi's internal preview widget. **C21.5** ships
  yazi + a curated preview stack (`piper.yazi` + `glow.yazi` +
  `chafa.yazi`) + the `rimeterm-bridge.yazi` plugin under
  `~/.rimeterm/yazi/` and injects `YAZI_CONFIG_HOME` — so previews and
  Alt+V both work out-of-the-box without touching your system Yazi
  config. See [docs/yazi-setup.md](docs/yazi-setup.md).
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

Every archive at [latest release] contains:

```
rimeterm-<version>-<target>/
├── rimeterm(.exe)
├── rimectl(.exe)
├── essentials/            ← prebuilt yazi + ya + gitui + btm + VERSIONS.toml
├── LICENSE
├── README.md
└── ACKNOWLEDGEMENTS.md
```

**The `essentials/` folder MUST sit next to `rimeterm(.exe)`** — first
launch reads it via `env::current_exe()` and copies the bundled
binaries into `~/.rimeterm/bin/`. Copying only `rimeterm` to
`/usr/local/bin/` skips essentials extraction and Yazi / gitui / bottom
will show placeholder panes instead of spawning.

#### Windows (PowerShell)

```powershell
$dst = "$env:LOCALAPPDATA\Programs\rimeterm"
New-Item -ItemType Directory -Force -Path $dst | Out-Null
Expand-Archive rimeterm-<version>-x86_64-pc-windows-msvc.zip -DestinationPath $dst -Force
# Add to PATH so `rimeterm` / `rimectl` are launchable from any shell:
$path = [Environment]::GetEnvironmentVariable("Path", "User")
if ($path -notlike "*$dst*") {
    [Environment]::SetEnvironmentVariable("Path", "$path;$dst\rimeterm-<version>-x86_64-pc-windows-msvc", "User")
}

# One-time yazi MIME-detection prereq (Git for Windows ships file.exe):
winget install --id Git.Git
```

Restart your terminal so `PATH` refreshes, then run `rimeterm`.

#### macOS / Linux

```bash
# Extract to a directory rimeterm can call home. NOT /usr/local/bin/
# — the whole folder must live together.
tar -xzf rimeterm-<version>-<triple>.tar.gz -C ~/.local/opt/

# Put the launcher (not the folder) on PATH via symlinks that preserve
# the sibling essentials/ folder:
dir=~/.local/opt/rimeterm-<version>-<triple>
ln -sf "$dir/rimeterm" ~/.local/bin/rimeterm
ln -sf "$dir/rimectl"  ~/.local/bin/rimectl
```

`env::current_exe()` follows the symlinks back to `$dir/rimeterm`, so
the essentials sibling stays reachable.

#### Native installers (`.msi` / `.deb` / `.pkg`)

All three installers bundle the essentials sibling (`yazi` / `gitui`
/ `bottom`) alongside the two rimeterm binaries, so a fresh install
has the same feature set as extracting the archive.

**Windows (`.msi`)** — `winget install` friendly (once submitted).
Drops everything under `C:\Program Files\rimeterm\` and adds that dir
to MACHINE `PATH`. A Start-menu shortcut launches `rimeterm.exe`.

```powershell
msiexec /i rimeterm-<version>-x86_64.msi /qb
# then, in a new shell:
rimeterm
```

**Linux (`.deb`)** — `apt install ./rimeterm-<version>_amd64.deb`.
Payload lands at `/usr/lib/rimeterm/`; postinst symlinks
`/usr/bin/rimeterm` and `/usr/bin/rimectl` back to those files so
they're on the system `PATH`. `env::current_exe()` follows the
symlinks and finds the essentials sibling automatically.

**macOS (`.pkg`)** — right-click Open on first launch (unsigned).
Payload at `/usr/local/lib/rimeterm/`; postinstall symlinks
`/usr/local/bin/rimeterm` and `/usr/local/bin/rimectl`. Same
symlink-then-current_exe trick.

### From source

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
```

**Dev builds only**: essentials (yazi/gitui/bottom) are bundled by the
release CI, not by `cargo`. If you're running from a checkout, populate
`target/{debug,release}/essentials/` once with
`node bootstrap-essentials.mjs` (requires Node ≥ 18). Rimeterm boots
fine without them — detection falls through to `$PATH` — but the four
quadrant tabs will show placeholder panes if `$PATH` is also empty.

## Bundled essentials + on-demand plugins (C21.5)

**Essentials** (`yazi`, `gitui`, `bottom`) ship in the release archive
as prebuilt binaries. First launch extracts them to `~/.rimeterm/bin/`
and seeds `~/.rimeterm/{yazi,gitui,bottom}/` with curated configs
(including a Yazi preview stack: `piper.yazi` + `glow.yazi` +
`chafa.yazi` + our own `rimeterm-bridge.yazi`). Nothing to install
manually. Upgrade path is "install a newer rimeterm release".

**Plugins** (`trippy` today, user-added tomorrow) live under
`~/.rimeterm/plugins/<name>/` and are installed on demand via
`tools.install`:

| kind | shipped as | dir |
|---|---|---|
| yazi (file manager) | essential | `~/.rimeterm/bin/yazi` + `~/.rimeterm/yazi/` |
| gitui | essential | `~/.rimeterm/bin/gitui` + `~/.rimeterm/gitui/` |
| bottom (sysmon) | essential | `~/.rimeterm/bin/btm` + `~/.rimeterm/bottom/` |
| trippy (traceroute) | plugin | `~/.rimeterm/plugins/trippy/{bin,config}/` — `cargo install --locked --root ~/.rimeterm/plugins/trippy trippy` |

**In-app shortcut**: on a plugin placeholder pane (tool not installed),
press `[I]` to run `cargo install --locked --root <plugin dir>
<crate>` — output is piped into a fresh shell tab.

**Prefer your system yazi/gitui/bottom instead?** Set
`[install.essentials] prefer_system = ["yazi", "gitui", "bottom"]` in
`~/.rimeterm/config.toml`; rimeterm skips extraction and falls through
to `$PATH` for those entries. External shells outside rimeterm are
never touched either way.

Agents (`omp` / `claude` / `codex` / `pi` / …) live off `npm` / `pip` /
binary releases — see `rimectl agents.list` for install hints per
entry. They are not managed by rimeterm.

## Build

```bash
cargo check --workspace
node bootstrap-essentials.mjs      # one-shot: fetch yazi/gitui/bottom
cargo run   --bin rimeterm         # launch the terminal
cargo run   --bin probe-shell      # print which shell would be picked
cargo test  --workspace
```

Requires Rust ≥ 1.90 (edition 2024) and Node ≥ 18 for the essentials
bootstrap. Windows uses ConPTY (Win10 1809+); Linux / macOS build the
same tree via `portable-pty`.

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
rimectl --pid $PID --wait 'test result:' --pane $PANE --timeout-ms 60000
rimectl --pid $PID workspace.pane.close --json "{\"pane_id\": $PANE}"
```

Every command is listed by `rimectl help`. Selected highlights:

- `workspace.pane.open {kind}` — `shell` or `agent:<id>` (where `<id>` ∈
  registry: `omp` / `codex` / `claude` / `pi`).
- `workspace.pane.wait {pane_id, pattern, timeout_ms?<=60000}` —
  server-side regex poll; returns as soon as `pattern` matches or the
  deadline hits. Client-side sugar: `rimectl --wait <regex> --pane <id>
  [--timeout-ms N] [--poll-ms N]` (exits non-zero on timeout, so `&&`
  chains break cleanly).
- `workspace.layout.reset {group?}` — reset split ratios. No args
  resets every split and clears `layout.state.toml`. `{"group":"files"|
  "sysmon"|"agents"|"shells"}` scopes the reset to that group's cell
  and re-persists other overrides.
- `file.selected` / `cwd.changed` — PTY plugins emit these through
  `ESC ] 1337 ; rimeterm ; <json> ST` (or BEL-terminated OSC); rimeterm
  decodes them and broadcasts `KernelEvent::FileSelected` /
  `KernelEvent::YaziCwdChanged` with the originating pane id. Unknown
  event names are ignored for forward compatibility.
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
- **State**: `~/.rimeterm/data/workspaces/<hash>/layout.state.toml` (only
  split-ratio overrides that differ from defaults; file is deleted when
  all splits return to defaults) and `agents.state.toml` (which agents to
  reopen).
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
- **[yazi], [gitui], [bottom], [trippy]** — the TUI tools rimeterm hosts
  by default.
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
