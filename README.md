# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside any modern terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — with a first-class slot for the agent.

<img width="2378" height="1385" alt="image" src="https://github.com/user-attachments/assets/cca12764-924c-446c-8147-f4a2b38ef017" />


## Layout

One screen, four zones in a 2×2 grid:

- **Top-left · files** — native dual-pane file manager (`tui-file-explorer` fork). Alt+V opens a modal viewer over the whole left column for the file under the cursor (markdown / code / image).
- **Bottom-left · git** — native Git snapshot powered by `gix` / `gix-diff` with tree-sitter diff highlighting; auto-refreshes when the files column changes directory. `bottom` shares this group as a pinned read-only tab.
- **Top-right · agents** — coding agent picked at runtime from whatever's on `$PATH` (`omp` / `codex` / `claude` / `pi` …).
- **Bottom-right · shells** — interactive shells (pwsh / bash / fish); add more with `Ctrl+Shift+T`.

Every zone is tabbed and hot-swappable; layout ratios and agent choice persist per workspace.

## What's inside

- **Agent-first** — auto-detects coding-agent CLIs on `$PATH`, picker in the app menu and command palette (`Ctrl+P`).
- **Native files + git** — zero external processes for the left column; `bottom` is the only bundled essential (self-installs into `~/.rimeterm/` on first launch).
- **`rimectl` IPC** — line-delimited JSON over a named pipe (Windows) or Unix socket. Every UI command is scriptable from tests, git hooks, or *other* agents.
- **Native mouse** — click / drag / scroll on tabs, dividers, selections, and shell prompts. Right-click is context-aware.
- **Themes** — 8 curated palettes (`Alt+T` cycles), applied uniformly across chrome and the markdown viewer.
- **Windows first-class** — ConPTY backend, Nerd Font fallback, MSI installer, and an Explorer right-click entry that opens the clicked folder as the workspace root.

## Install

Grab the archive for your platform from the [latest release] — MSI on Windows, `.deb` on Debian/Ubuntu, `.pkg` on macOS, or a plain `.tar.gz`/`.zip` extractable anywhere. Keep the `essentials/` sibling next to the launcher.

From source:

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
# dev builds only — drop essentials next to target/
node bootstrap-essentials.mjs
```

Then run `rimeterm` from any terminal.

## More

- Third-party attributions: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0, see [`LICENSE`](LICENSE).

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
