# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside your existing terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — but with an agent-shaped hole in the middle.

> Built for one workflow — mine. The 4-pane layout below mirrors how I actually code all day: files on the left, agent + shell on the right, git status glancing back at me. No off-the-shelf terminal or multiplexer does this out of the box, so I ended up assembling one from `yazi` + `gitui` + `bottom` + whichever coding agent I'm using this week. Sharing in case someone else's habits look like mine.

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/a524d1d5-e9c8-4895-8ef6-40cd4f1ab0e3" />

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/50f41ee0-202a-4c1a-b9ed-096f5ce8337d" />


## Why

Coding agents (Codex, Claude Code, `omp`, …) live in the terminal but the terminal wasn't built for them. `tmux` treats an agent as just another shell; every other TUI multiplexer treats it as a chat window bolted on. rimeterm inverts that: **the agent is a first-class pane**, sitting next to the file manager and shells that give it the context it needs.

The whole thing is scriptable — a companion CLI (`rimectl`) speaks JSON over a named pipe / socket, so tests, git hooks, and *other* agents can drive the UI.

## The layout

One screen, four panes in a 2×2 grid:

- **Top-left** — file manager (yazi), with a modal Alt+V viewer for the file under the cursor (markdown / code / image).
- **Bottom-left** — git status (gitui), auto-refreshed when the workspace repo changes (external commits, agent rebases, etc.).
- **Top-right** — coding agent picked at runtime from whatever's on `$PATH` (Claude Code, Codex, `omp`, `pi`, …).
- **Bottom-right** — system monitor (bottom) as the pinned first tab, plus interactive shells (pwsh / bash / fish) in subsequent tabs.

Every zone is tabbed and hot-swappable. Layout ratios and agent choice persist per workspace.

## What's inside

- **Coding agents as panes** — auto-detects `omp` / `codex` / `claude` / `pi` on `$PATH`; the picker lives in the app menu and the command palette.
- **Bundled essentials** — `yazi` / `gitui` / `bottom` + `bat` / `glow` / `chafa` ship in every release archive and self-install into `~/.rimeterm/` on first launch. No package-manager dance to get the file manager working.
- **Native mouse** — click / drag / scroll on tabs, dividers, selections, and shell prompts. Right-click is context-aware.
- **Themes** — 8 curated palettes (`Alt+T` cycles), applied uniformly across chrome, borders, and the markdown viewer.
- **`rimectl` IPC** — line-delimited JSON over a named pipe (Windows) or Unix socket. Every UI command is scriptable.
- **Windows first-class** — ConPTY backend, Nerd Font fallback, `.msi` installer, and (v0.1.17) a one-click Explorer right-click entry that opens the clicked folder as the workspace root.

## Install

Grab the archive for your platform from the [latest release] — MSI on Windows, `.deb` on Debian/Ubuntu, `.pkg` on macOS, or a plain `.tar.gz`/`.zip` you can extract anywhere. Keep the `essentials/` sibling next to the launcher.

From source:

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
# dev builds only — fetch essentials next to the target/ binary
node bootstrap-essentials.mjs
```

Then run `rimeterm` from any terminal.

## More

- Full design doc, keybindings, config, and IPC schema: **[docs/rimeterm-overall-design.md](docs/rimeterm-overall-design.md)**
- Third-party attributions: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0, see [`LICENSE`](LICENSE).

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
