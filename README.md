# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside your existing terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — but with an agent-shaped hole in the middle.

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/a524d1d5-e9c8-4895-8ef6-40cd4f1ab0e3" />

<img width="1280" height="800" alt="image" src="https://github.com/user-attachments/assets/50f41ee0-202a-4c1a-b9ed-096f5ce8337d" />


## Why

Coding agents (Codex, Claude Code, `omp`, …) live in the terminal but the terminal wasn't built for them. `tmux` treats an agent as just another shell; every other TUI multiplexer treats it as a chat window bolted on. rimeterm inverts that: **the agent is a first-class pane**, sitting next to the file manager and shells that give it the context it needs.

The whole thing is scriptable — a companion CLI (`rimectl`) speaks JSON over a named pipe / socket, so tests, git hooks, and *other* agents can drive the UI.

## The layout

One screen, three zones:

- **Left** — file manager (yazi) full-height, with a markdown / image viewer overlay for the file under the cursor.
- **Right-top** — coding agent picked at runtime from whatever's on `$PATH`.
- **Right-bottom** — system monitor + shell tabs (pwsh / bash / fish).

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

## Recent changes

### 0.1.23 — every pane shows its cursor

- **BUG-2 fix** — `PtyPane` now paints a **cell-level reverse-video cursor block** at every render, independent of focus. Previously the only visible cursor was the OS caret owned by the focused pane, so opening the Alt+V viewer (which takes the caret) made shell/agent panes look "dead". Now every PTY keeps a visible cursor block regardless of which overlay is on screen — matching tmux / wezterm / foot behaviour. See `overlay_cursor_cell` in `crates/rimeterm-tui/src/pty_pane.rs`.

### 0.1.22 — viewer perf + gitui refresh

- **Viewer** — parsed Markdown and syntect state are cached per snapshot (checkpoints every 500 lines) and the selection-mode buffer capture only fires when a selection is actually active. Scrolling a large `.md` / `.rs` in the Alt+V viewer is now flat-CPU. See `docs/rimeterm-upgrade-design.md` §P1.
- **gitui refresh** — a bare `F5` now force-respawns the gitui pane at the current workspace root, and an `.git/`-scoped `notify` watcher (300 ms debounce + 500 ms settle window) auto-refreshes it after external `git commit` / `checkout` / rebase from a sibling shell. Eliminates the "gitui shows stale HEAD" and "gitui flickers wildly after switching folders" reports.
- **Renderer** — `PtyPane::render` skips the full `alacritty::display_iter` blit when the terminal has no damage and nothing tracked has changed (focus / area / scroll / selection). The main loop also skips `Terminal::draw()` on idle 16 ms ticks. Idle rimeterm CPU drops from ~3 % to ~0.1 %.
- **BUG-2 guard** — `decide_frame_cursor` is now a pure function with regression tests locking in "viewer overlay MUST NOT hide the shell/agent caret when it was invoked from the right column".

## More

- Full design doc, keybindings, config, and IPC schema: **[docs/rimeterm-overall-design.md](docs/rimeterm-overall-design.md)**
- Third-party attributions: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0, see [`LICENSE`](LICENSE).

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
