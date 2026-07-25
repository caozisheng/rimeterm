# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside your existing terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — but with an agent-shaped hole in the middle.

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/0af3a44d-0991-4b26-81cf-2163c2198d11" />

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/b64823c6-cf82-484b-88a3-685f814f5558" />


| | |
|---|---|
| **License** | Apache-2.0 · see [`LICENSE`](LICENSE) |
| **CI** | [![CI](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/caozisheng/rimeterm/actions/workflows/ci.yml) Linux · macOS · Windows |
| **Releases** | [Latest](https://github.com/caozisheng/rimeterm/releases/latest) — archives + `.msi` / `.deb` / `.pkg` installers, essentials bundled |
| **MSRV** | Rust 1.90 (edition 2024) |
| **Status** | v0.1.15 — app-wide theme picker (`Alt+T` cycles Default / Dracula / Solarized / Nord / Gruvbox / GitHub Light) + chrome polish |

---

## What it is

One screen, three zones:

- **Left** — file manager (yazi) full-height with a markdown / image viewer overlay.
- **Right-top** — coding agents (`omp` / `codex` / `claude` / `pi`), picked at runtime.
- **Right-bottom** — system monitor (bottom) + shell tabs (pwsh / bash / fish).

Every zone is tabbed and hot-swappable. Panes talk to the outside world through `rimectl` — a scriptable IPC surface so tests, git hooks, and other agents can drive the UI.

Not a terminal emulator competing with WezTerm / Alacritty / kitty. It runs *inside* one.

## Highlights

- **Coding agents as first-class panes** — pick from any agent detected on `$PATH`; the choice persists per workspace.
- **8 curated themes** — `Alt+T` cycles Default → Dracula → Solarized {Dark, Light} → Nord → Gruvbox {Dark, Light} → GitHub Light. Focus / border / hover tints follow in lock-step.
- **Mouse everywhere** — click, drag, scroll on tabs / dividers / selections / shell prompts. Right-click for a context-aware menu.
- **`rimectl` IPC** — line-delimited JSON over named pipe (Windows) / uds (Linux/macOS). Every UI command is scriptable.
- **Windows first-class** — ConPTY, Nerd Font fallback, native `.msi` installer.
- **Essentials bundled** — `yazi` / `gitui` / `bottom` + `bat` / `glow` / `chafa` ship in every release archive and extract to `~/.rimeterm/` on first launch.

## Install

Download the archive for your platform from the [latest release] and extract it somewhere the whole folder can live together (the `essentials/` sibling must stay next to the launcher).

Native installers are also available:

- **Windows** — `rimeterm-<version>-x86_64.msi`
- **Debian / Ubuntu** — `rimeterm-<version>_amd64.deb`
- **macOS** — `rimeterm-<version>.pkg`

Then run `rimeterm` from any terminal. First launch seeds `~/.rimeterm/` with curated configs for the bundled essentials.

From source:

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
```

Dev builds need `node bootstrap-essentials.mjs` once to fetch essentials locally.

## Keybindings

| key | action |
|---|---|
| `F1` / `Ctrl+Shift+P` | Command palette |
| `F10` / `Alt+M` | App menu |
| `F9` | Pane menu (context-aware) |
| `Alt+1..3` | Focus zone (1=left · 2=agents · 3=shells) |
| `Alt+H/J/K/L` | Focus left / down / up / right cell |
| `Alt+[/]` / `Ctrl+PgUp/PgDn` | Previous / next tab |
| `Alt+Shift+1..9` | Jump to tab N in focused group |
| `Ctrl+T` | New tab (shell / agent picker depending on zone) |
| `Ctrl+W` | Close current tab |
| `Ctrl+Alt+R` | Keyboard resize mode |
| `Alt+T` | Cycle UI theme |
| `Alt+V` | Freeze yazi selection into viewer overlay |
| `Ctrl+= / -` | Host terminal font zoom (Windows Terminal / kitty / iTerm2) |
| `Ctrl+Q` | Quit |

Mouse: click / drag tabs, dividers, and shell selections; right-click for a context menu; middle-click to paste; hold `Shift` to force local selection inside full-screen TUI apps.

## Configuration

- **User global** — `~/.rimeterm/config.toml` (override root with `$RIMETERM_HOME`).
- **Project** — `<workspace>/.rimeterm/config.toml` (check into git for team defaults).
- **State** — `~/.rimeterm/data/workspaces/<hash>/` (layout ratios, agent choices).

Prefer your system `yazi` / `gitui` / `bottom` instead of the bundled ones? Set `[install.essentials] prefer_system = ["yazi", "gitui", "bottom"]` in `config.toml`.

## Docs

- [Design overview](docs/rimeterm-overall-design.md) — architecture, pane model, contracts.
- [Yazi setup](docs/yazi-setup.md) — the bundled preview pipeline.
- [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md) — full third-party dependency list.

## License

Apache License 2.0. Contributions accepted under the same terms.

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
